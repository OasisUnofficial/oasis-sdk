use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    sync::{Arc, RwLock},
    time::{Instant, SystemTime},
};

use anyhow::{anyhow, Context as _};
use backoff::backoff::Backoff;
use base64::{prelude::BASE64_STANDARD, Engine};
use bytes::BufMut;
use futures_util::StreamExt;
use oasis_runtime_sdk::{
    core::{
        common::{
            crypto::{hash::Hash, signature::PublicKey},
            logger::get_logger,
            process,
        },
        host::{bundle_manager, volume_manager},
    },
    types::address::Address,
};
use oasis_runtime_sdk_rofl_market::{
    self as market,
    policy::{ProviderLabel, LABEL_PROVIDER},
    types::{Deployment, Instance, InstanceId, InstanceStatus},
};
use rand::Rng;
use rofl_app_core::prelude::*;
use sha2::{Digest, Sha512_256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time,
};
use tokio_util::compat::FuturesAsyncWriteCompatExt;

use super::{
    client::{MarketClient, MarketQueryClient},
    config::{LocalConfig, Resources},
    manifest::{self, Manifest},
    qcow2, types, SchedulerApp,
};

/// Metadata key used to configure the offer identifier.
const METADATA_KEY_OFFER: &str = "net.oasis.scheduler.offer";
/// Metadata key used to configure the deployment ORC bundle location.
const METADATA_KEY_DEPLOYMENT_ORC_REF: &str = "net.oasis.deployment.orc.ref";
/// Metadata key used to report errors.
const METADATA_KEY_ERROR: &str = "net.oasis.error";
/// Maximum length of the error message.
const METADATA_VALUE_ERROR_MAX_SIZE: usize = 1024;
/// Metadata key used to store the scheduler instance RAK.
const METADATA_KEY_SCHEDULER_RAK: &str = "net.oasis.scheduler.rak";

/// Name of the label used to store the deployment hash.
const LABEL_DEPLOYMENT_HASH: &str = "net.oasis.scheduler.deployment_hash";
/// Name of the label used to store the volume name.
const LABEL_VOLUME_NAME: &str = "net.oasis.scheduler.volume.name";

/// OCI media type for ORC config descriptors.
const OCI_TYPE_ORC_CONFIG: &str = "application/vnd.oasis.orc.config.v1+json";
/// OCI media type for ORC layer descriptors.
const OCI_TYPE_ORC_LAYER: &str = "application/vnd.oasis.orc.layer.v1";

/// Average number of seconds after which to remove instances that are not accepted. The scheduler
/// will randomize the value to minimize the chance of multiple schedulers removing at once.
const REMOVE_INSTANCE_AFTER_SECS: u64 = 1800;
/// Maximum size of the JSON-encoded ORC manifest.
const MAX_ORC_MANIFEST_SIZE: i64 = 16 * 1024; // 16 KiB
/// Maximum size of an ORC layer.
const MAX_ORC_LAYER_SIZE: i64 = 128 * 1024 * 1024; // 128 MiB
/// Maximum size of all ORC layers.
const MAX_ORC_TOTAL_SIZE: i64 = 128 * 1024 * 1024; // 128 MiB

/// OCI client read timeout in seconds.
const OCI_CLIENT_READ_TIMEOUT_SECS: u64 = 5;
/// OCI client connect timeout in seconds.
const OCI_CLIENT_CONNECT_TIMEOUT_SECS: u64 = 5;
/// OCI client manifest and config pull timeout in seconds.
const OCI_CLIENT_PULL_MANIFEST_TIMEOUT_SECS: u64 = 5;

#[derive(Clone, Default)]
struct InstanceUpdates {
    complete_cmds: Option<market::types::CommandId>,
    deployment: Option<Option<market::types::Deployment>>,
    metadata: Option<BTreeMap<String, String>>,
    node_id: Option<PublicKey>,
}

impl InstanceUpdates {
    /// Whether there are any updates set.
    fn has_updates(&self) -> bool {
        self.complete_cmds.is_some()
            || self.deployment.is_some()
            || self.metadata.is_some()
            || self.node_id.is_some()
    }
}

struct LocalState {
    /// Market query client instance for a specific round.
    client: Arc<MarketQueryClient>,
    /// A map of all accepted instances.
    accepted: BTreeMap<InstanceId, Instance>,
    /// A list of our bundles running locally.
    running: BTreeMap<InstanceId, bundle_manager::BundleInfo>,
    /// A list of deployments that should already be running but are not.
    pending_start: Vec<(Instance, Deployment, bool)>,
    /// A list of instance identifiers that should have no running deployments.
    pending_stop: Vec<(InstanceId, bool)>,
    /// A map of instance updates.
    instance_updates: BTreeMap<InstanceId, InstanceUpdates>,
    /// A list of instance identifiers that should be accepted.
    accept: Vec<InstanceId>,
    /// A list of not-accepted instance identifiers and timestamps that should maybe be removed.
    maybe_remove: Vec<(InstanceId, u64)>,
    /// A list of instances to claim payment for.
    claim_payment: Vec<InstanceId>,
    /// Amounts of resources used.
    resources_used: Resources,
}

/// Instance state.
#[derive(Default, Debug, Clone)]
pub struct InstanceState {
    /// Address of the instance administrator.
    admin: Address,
    /// Last deployment.
    last_deployment: Option<Deployment>,
    /// Last error message corresponding to deploying `last_deployment`.
    last_error: Option<String>,
    // Whether to ignore instance start until the given time elapses.
    ignore_start_until: Option<Instant>,
    /// Backoff associated with ignoring instance start.
    ignore_start_backoff: Option<backoff::ExponentialBackoff>,
}

impl InstanceState {
    /// Address of the instance administrator.
    pub fn admin(&self) -> &Address {
        &self.admin
    }
}

struct DeploymentInfo {
    temporary_name: String,
    manifest_hash: Hash,
    volumes: Vec<String>,
}

/// Instance manager.
pub struct Manager {
    env: Environment<SchedulerApp>,
    client: Arc<MarketClient>,
    cfg: Arc<LocalConfig>,

    instances: RwLock<BTreeMap<InstanceId, InstanceState>>,

    logger: slog::Logger,
}

impl Manager {
    /// Create a new manager instance.
    pub fn new(env: Environment<SchedulerApp>, cfg: Arc<LocalConfig>) -> Arc<Self> {
        Arc::new(Self {
            client: Arc::new(MarketClient::new(env.clone(), cfg.provider_address)),
            env,
            cfg,
            instances: RwLock::new(BTreeMap::new()),
            logger: get_logger("scheduler/manager"),
        })
    }

    /// Find an instance state with the given identifier and return a copy.
    pub fn get_instance(&self, instance_id: &InstanceId) -> Option<InstanceState> {
        let instances = self.instances.read().unwrap();
        instances.get(instance_id).cloned()
    }

    /// Main loop of the ROFL scheduler.
    pub async fn run(self: Arc<Self>) {
        let local_node_id = match self.env.host().identity().await {
            Ok(local_node_id) => local_node_id,
            Err(err) => {
                slog::error!(self.logger, "failed to determine local node ID";
                    "err" => ?err,
                );
                process::abort();
            }
        };
        let mut last_round = 0;

        loop {
            // Wait a bit before doing another pass.
            time::sleep(time::Duration::from_secs(self.cfg.processing_interval_secs)).await;

            // Discover local state.
            let mut local_state = match self.discover(local_node_id).await {
                Ok(local_state) => local_state,
                Err(err) => {
                    slog::error!(self.logger, "failed to discover bundles"; "err" => ?err);
                    continue;
                }
            };

            // Make sure to not re-process the same round multiple times.
            if local_state.client.round() <= last_round {
                continue;
            }

            // Process any pending instances.
            if let Err(err) = self.process_pending(local_node_id, &mut local_state).await {
                slog::error!(self.logger, "failed to process pending instances"; "err" => ?err);
                continue;
            }

            slog::info!(self.logger, "instance status";
                "accepted" => local_state.accepted.len(),
                "running" => local_state.running.len(),
                "pending_start" => local_state.pending_start.len(),
                "pending_stop" => local_state.pending_stop.len(),
                "instance_updates" => local_state.instance_updates.len(),
                "maybe_remove" => local_state.maybe_remove.len(),
                "claim_payment" => local_state.claim_payment.len(),
                "resources_used" => ?local_state.resources_used,
            );

            // Spawn tasks to process all jobs.
            if let Err(err) = self.process_jobs(&mut local_state).await {
                slog::error!(self.logger, "failed to process jobs"; "err" => ?err);
                continue;
            }

            last_round = local_state.client.round();
        }
    }

    /// Discover local state.
    async fn discover(&self, local_node_id: PublicKey) -> Result<LocalState> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Encode scheduler RAK so we can set it in metadata.
        let scheduler_rak = BASE64_STANDARD.encode(self.env.identity().public_rak().as_ref());

        let client = self.client.queries_at_latest().await?;
        let mut local_state = LocalState {
            client,
            accepted: BTreeMap::new(),
            running: BTreeMap::new(),
            pending_start: Vec::new(),
            pending_stop: Vec::new(),
            instance_updates: BTreeMap::new(),
            accept: Vec::new(),
            maybe_remove: Vec::new(),
            claim_payment: Vec::new(),
            resources_used: Default::default(),
        };

        // Discover local volumes.
        let rsp = self
            .env
            .host()
            .volume_manager()
            .volume_list(volume_manager::VolumeListRequest {
                labels: BTreeMap::new(), // We want all our volumes.
            })
            .await?;
        let volumes = rsp.volumes.into_iter().filter_map(|bi| {
            // Skip volumes with malformed labels.
            let instance_id: InstanceId = bi
                .labels
                .get(bundle_manager::LABEL_INSTANCE_ID)?
                .parse()
                .ok()?;
            Some(instance_id)
        });

        // Discover local bundles.
        let rsp = self
            .env
            .host()
            .bundle_manager()
            .bundle_list(bundle_manager::BundleListRequest {
                labels: BTreeMap::new(), // We want all our bundles.
            })
            .await?;
        local_state.running = rsp
            .bundles
            .into_iter()
            .filter_map(|bi| {
                // Skip bundles with malformed labels.
                let instance_id: InstanceId = bi
                    .labels
                    .get(bundle_manager::LABEL_INSTANCE_ID)?
                    .parse()
                    .ok()?;
                Some((instance_id, bi))
            })
            .collect();
        let mut running_unknown =
            BTreeSet::from_iter(local_state.running.keys().copied().chain(volumes));

        // Discover desired instance state.
        let instances: Vec<Instance> = local_state.client.instances().await?;
        for instance in instances {
            match instance.status {
                InstanceStatus::Created => {
                    // Instance has not yet been accepted, nothing to do.
                    continue;
                }
                InstanceStatus::Cancelled => {
                    // Instance has been cancelled.
                    local_state
                        .maybe_remove
                        .push((instance.id, instance.updated_at));
                    continue;
                }
                InstanceStatus::Accepted => {
                    // Instance has been accepted, check if we should be hosting it.
                    // NOTE: Safe to unwrap as all accepted instances must have a node set.
                    if instance.node_id.unwrap() != local_node_id {
                        continue;
                    }
                }
            }

            // Remove known instances. Any remaining unknown instances will be stopped.
            running_unknown.remove(&instance.id);

            // Check if the instance is still paid for. If not, we immediately stop it and schedule
            // its removal.
            if instance.paid_until < now {
                slog::info!(self.logger, "instance not paid for, stopping";
                    "id" => ?instance.id,
                );

                if local_state.running.contains_key(&instance.id) {
                    local_state.pending_stop.push((instance.id, true));
                }
                local_state
                    .maybe_remove
                    .push((instance.id, instance.paid_until));
                continue;
            }

            local_state.accepted.insert(instance.id, instance.clone());

            // Update administrator address.
            {
                let mut instances = self.instances.write().unwrap();
                instances.entry(instance.id).or_default().admin = instance.admin;
            }

            // Compute total provisioned resources.
            local_state.resources_used = local_state.resources_used.add(&instance.resources);

            // Discover any pending commands to see if there is a "deploy" command somewhere in
            // there. This allows us to immediately deploy the right thing instead of first
            // deploying an old version and then immediately upgrading.
            let cmds = local_state.client.instance_commands(instance.id).await?;

            // Derive the desired instance state.
            let mut wipe_storage = false;
            let mut force_restart = false;
            let mut last_processed_cmd = Default::default();
            let mut desired = instance.deployment.clone();

            for qc in &cmds {
                last_processed_cmd = qc.id;

                let cmd = match cbor::from_slice::<types::Command>(&qc.cmd) {
                    Ok(cmd) => cmd,
                    Err(_) => continue,
                };

                match cmd.method.as_str() {
                    types::METHOD_DEPLOY => {
                        match cbor::from_value::<types::DeployRequest>(cmd.args) {
                            Ok(deploy) => {
                                desired = Some(deploy.deployment);
                                wipe_storage = wipe_storage || deploy.wipe_storage;
                            }
                            Err(_) => continue,
                        }
                    }
                    types::METHOD_TERMINATE => {
                        match cbor::from_value::<types::TerminateRequest>(cmd.args) {
                            Ok(terminate) => {
                                desired = None;
                                wipe_storage = wipe_storage || terminate.wipe_storage;
                            }
                            Err(_) => continue,
                        }
                    }
                    types::METHOD_RESTART => {
                        match cbor::from_value::<types::RestartRequest>(cmd.args) {
                            Ok(restart) => {
                                wipe_storage = wipe_storage || restart.wipe_storage;
                                force_restart = true;
                            }
                            Err(_) => continue,
                        }
                    }
                    _ => continue,
                }
            }

            if !cmds.is_empty() {
                local_state
                    .instance_updates
                    .entry(instance.id)
                    .or_default()
                    .complete_cmds = Some(last_processed_cmd);
            }

            // Make sure that metadata is updated after processing all the commands.
            if instance.deployment != desired {
                local_state
                    .instance_updates
                    .entry(instance.id)
                    .or_default()
                    .deployment = Some(desired.clone());
            }

            // Make sure that scheduler RAK is set to the correct value.
            if instance.metadata.get(METADATA_KEY_SCHEDULER_RAK) != Some(&scheduler_rak) {
                local_state
                    .instance_updates
                    .entry(instance.id)
                    .or_default()
                    .metadata
                    .get_or_insert_default()
                    .insert(
                        METADATA_KEY_SCHEDULER_RAK.to_string(),
                        scheduler_rak.clone(),
                    );
            }

            // If the instance has been running for a while, make sure to claim payment. Use a fuzzy
            // interval to distribute claims a bit.
            let timeout = rand::distributions::Uniform::new(75, 125);
            let payment_interval =
                (self.cfg.claim_payment_interval_secs * rand::thread_rng().sample(timeout)) / 100;
            if now > instance.paid_from.saturating_add(payment_interval) {
                local_state.claim_payment.push(instance.id);
            }

            let actual = local_state.running.get(&instance.id);
            match (actual, desired) {
                (Some(actual), Some(desired)) => {
                    // Instance is running and should be running. Determine whether it is running
                    // the correct deployment by comparing its hash.
                    let actual_hash = actual
                        .labels
                        .get(LABEL_DEPLOYMENT_HASH)
                        .cloned()
                        .unwrap_or_default();
                    let desired_hash = deployment_hash(&desired);

                    if actual_hash != desired_hash || force_restart {
                        // Note that any old instances will be restarted in case they are already
                        // running and we add them to `pending_start`.
                        local_state
                            .pending_start
                            .push((instance, desired, wipe_storage));
                    }
                }
                (None, Some(desired)) => {
                    // Instance is not running and should be started.
                    local_state
                        .pending_start
                        .push((instance, desired, wipe_storage));
                }
                (Some(_), None) => {
                    // Instance is running and should be stopped.
                    local_state.pending_stop.push((instance.id, wipe_storage));
                }
                (None, None) => {
                    // Instance is not running and should be stopped. Nothing to do.
                }
            }
        }

        // Stop any unknown instances.
        for instance_id in running_unknown {
            slog::info!(self.logger, "stopping unknown instance";
                "id" => ?instance_id,
            );

            local_state.pending_stop.push((instance_id, true));
        }

        slog::info!(self.logger, "discovered instances";
            "accepted" => local_state.accepted.len(),
            "running" => local_state.running.len(),
            "pending_start" => local_state.pending_start.len(),
            "pending_stop" => local_state.pending_stop.len(),
            "instance_updates" => local_state.instance_updates.len(),
            "maybe_remove" => local_state.maybe_remove.len(),
            "claim_payment" => local_state.claim_payment.len(),
            "resources_used" => ?local_state.resources_used,
        );

        Ok(local_state)
    }

    /// Process pending instances.
    async fn process_pending(
        self: &Arc<Self>,
        local_node_id: PublicKey,
        local_state: &mut LocalState,
    ) -> Result<()> {
        let offers = local_state.client.offers().await?;
        let instances = local_state.client.instances().await?;

        let acceptable_offers: BTreeSet<market::types::OfferId> = offers
            .into_iter()
            .filter_map(|offer| {
                let offer_key = offer.metadata.get(METADATA_KEY_OFFER)?;

                if self.cfg.offers.is_empty() || self.cfg.offers.contains(offer_key) {
                    Some(offer.id)
                } else {
                    None
                }
            })
            .collect();

        for instance in instances {
            let mut transfer_instance = false;
            match instance.status {
                InstanceStatus::Created => {}
                InstanceStatus::Accepted => {
                    // If the instance has already been accepted, check if we are not the owning
                    // node but we should transfer from it.
                    // NOTE: Safe to unwrap as all accepted instances must have a node set.
                    let owning_node = instance.node_id.unwrap();
                    if owning_node == local_node_id {
                        continue;
                    }
                    if !self.cfg.should_transfer_instance_from(&owning_node) {
                        continue;
                    }

                    transfer_instance = true;
                }
                _ => continue,
            }

            let mut maybe_remove = || {
                if transfer_instance {
                    return;
                }

                local_state
                    .maybe_remove
                    .push((instance.id, instance.created_at))
            };

            slog::info!(self.logger, "evaluating instance";
                "id" => ?instance.id,
                "status" => ?instance.status,
                "transfer" => transfer_instance,
            );

            // Check if creator is among the allowed creators.
            if !self.cfg.is_creator_allowed(&instance.creator) {
                slog::info!(self.logger, "creator not allowed";
                    "id" => ?instance.id,
                    "creator" => instance.creator,
                    "transfer" => transfer_instance,
                );
                maybe_remove();
                continue;
            }

            // Check if offer is among the configured offers.
            if !acceptable_offers.contains(&instance.offer) {
                slog::info!(self.logger, "offer not acceptable for this instance";
                    "id" => ?instance.id,
                    "offer" => ?instance.offer,
                    "transfer" => transfer_instance,
                );
                maybe_remove();
                continue;
            }

            // Check if we have enough local capacity.
            let new_resource_use = local_state.resources_used.add(&instance.resources);
            if !self.cfg.capacity.can_allocate(&new_resource_use) {
                slog::info!(self.logger, "no more capacity for offer";
                    "id" => ?instance.id,
                    "offer" => ?instance.offer,
                    "transfer" => transfer_instance,
                );
                maybe_remove();
                continue;
            }

            slog::info!(self.logger, "instance seems acceptable";
                "id" => ?instance.id,
                "offer" => ?instance.offer,
                "transfer" => transfer_instance,
            );

            // Instance seems acceptable.
            local_state.accept.push(instance.id);
            local_state.accepted.insert(instance.id, instance.clone());
            local_state.resources_used = new_resource_use;

            // When transfering instances, queue a job to update their node ID.
            if transfer_instance {
                local_state
                    .instance_updates
                    .entry(instance.id)
                    .or_default()
                    .node_id = Some(local_node_id);
            }
        }

        Ok(())
    }

    /// Process queued jobs.
    async fn process_jobs(self: &Arc<Self>, local_state: &mut LocalState) -> Result<()> {
        // Prepare job to accept instances.
        let accept_jobs: Vec<_> = local_state
            .accept
            .chunks(16)
            .map(|ids| self.clone().accept_instances(ids.to_vec()))
            .collect();

        // Prepare jobs to remove instances.
        let remove_jobs: Vec<_> = local_state
            .maybe_remove
            .iter()
            .map(|(id, ts)| self.clone().maybe_remove_instance(*id, *ts))
            .collect();

        // Prepare jobs to start instances.
        let start_jobs: Vec<_> = local_state
            .pending_start
            .iter()
            .map(|(instance, deployment, wipe_storage)| {
                self.clone()
                    .start_instance(instance.clone(), deployment.clone(), *wipe_storage)
            })
            .collect();

        // Prepare jobs to stop instances.
        let stop_jobs: Vec<_> = local_state
            .pending_stop
            .iter()
            .map(|(id, wipe_storage)| self.clone().stop_instance(*id, *wipe_storage))
            .collect();

        // Prepare jobs to claim payments.
        let claim_payment_jobs: Vec<_> = local_state
            .claim_payment
            .chunks(16)
            .map(|chunk| self.clone().claim_payment(chunk.to_vec()))
            .collect();

        // Execute all jobs in parallel.
        let mut jobs = tokio::task::JoinSet::new();
        for job in accept_jobs {
            jobs.spawn(job);
        }
        for job in remove_jobs {
            jobs.spawn(job);
        }
        for job in start_jobs {
            jobs.spawn(job);
        }
        for job in stop_jobs {
            jobs.spawn(job);
        }
        for job in claim_payment_jobs {
            jobs.spawn(job);
        }

        slog::info!(self.logger, "running jobs"; "num_jobs" => jobs.len());

        while let Some(result) = jobs.join_next().await {
            match result {
                Err(err) => {
                    slog::error!(self.logger, "task panicked"; "err" => ?err);
                }
                Ok(Err(err)) => {
                    slog::error!(self.logger, "task failed"; "err" => ?err);
                }
                Ok(Ok(_)) => {
                    // Ok.
                }
            }
        }

        slog::info!(self.logger, "running instance update jobs");

        // After all instance jobs have completed, collect additional instance update jobs as those
        // depend on last instance status.
        let mut jobs = self.collect_instance_update_jobs(local_state);
        while let Some(result) = jobs.join_next().await {
            match result {
                Err(err) => {
                    slog::error!(self.logger, "instance update task panicked"; "err" => ?err);
                }
                Ok(Err(err)) => {
                    slog::error!(self.logger, "instance update task failed"; "err" => ?err);
                }
                Ok(Ok(_)) => {
                    // Ok.
                }
            }
        }

        slog::info!(self.logger, "all jobs completed");

        Ok(())
    }

    /// Inspect all instances and generate jobs to update their metadata.
    fn collect_instance_update_jobs(
        self: &Arc<Self>,
        local_state: &mut LocalState,
    ) -> tokio::task::JoinSet<Result<()>> {
        let instances = self.instances.read().unwrap();

        // Determine the set of instances that need to be updated. These are either ones that have
        // been explicitly requested by earlier phases or any that have errors set due to job
        // processing.
        let relevant_instances: BTreeSet<_> = local_state
            .instance_updates
            .keys()
            .copied()
            .chain(instances.keys().copied())
            .collect();

        // Iterate through all updates and fill in any unchanged fields from instances.
        const CHUNK_SIZE: usize = 16;
        let mut tasks = tokio::task::JoinSet::new();
        let mut chunk = Vec::with_capacity(CHUNK_SIZE);
        let mut spawn_task_chunk = |chunk: &mut Vec<_>| {
            let chunk = std::mem::replace(chunk, Vec::with_capacity(CHUNK_SIZE));
            tasks.spawn(self.clone().update_instance_metadata(chunk));
        };

        for instance_id in relevant_instances {
            let instance = match local_state.accepted.get(&instance_id) {
                Some(instance) => instance,
                None => continue, // Skip any instances that no longer exist.
            };
            let state = instances.get(&instance_id);
            let mut updates = local_state
                .instance_updates
                .remove(&instance_id)
                .unwrap_or_default();

            if updates.metadata.is_none() {
                updates.metadata = Some(instance.metadata.clone());
            }

            // Set last error metadata entry.
            if let Some(mut error) = state.and_then(|s| s.last_error.clone()) {
                error.truncate(METADATA_VALUE_ERROR_MAX_SIZE);

                updates
                    .metadata
                    .as_mut()
                    .unwrap()
                    .insert(METADATA_KEY_ERROR.to_string(), error);
            } else {
                updates
                    .metadata
                    .as_mut()
                    .unwrap()
                    .remove(METADATA_KEY_ERROR);
            }

            // If metadata would not change, do not update it.
            if updates.metadata.as_ref().unwrap() == &instance.metadata {
                updates.metadata = None;
            }

            // Skip updates that don't change anything.
            if !updates.has_updates() {
                continue;
            }

            chunk.push((instance.id, updates));
            if chunk.len() >= chunk.capacity() {
                spawn_task_chunk(&mut chunk);
            }
        }
        if !chunk.is_empty() {
            spawn_task_chunk(&mut chunk);
        }

        tasks
    }

    /// Accept the given instances.
    async fn accept_instances(self: Arc<Self>, ids: Vec<InstanceId>) -> Result<()> {
        self.client.accept_instances(ids, BTreeMap::new()).await
    }

    /// Maybe remove the given instances.
    async fn maybe_remove_instance(self: Arc<Self>, instance: InstanceId, ts: u64) -> Result<()> {
        // Determine whether the instance should be removed. We use a randomized interval to
        // minimize the chance of multiple schedulers removing the same instances.
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let timeout = rand::distributions::Uniform::new(75, 125);
        let max_delta = (REMOVE_INSTANCE_AFTER_SECS * rand::thread_rng().sample(timeout)) / 100;
        if now.saturating_sub(ts) < max_delta {
            return Ok(());
        }

        // XXX: Temporarily skip instances that cannot be removed due to a claim bug.
        let info = self
            .client
            .queries_at_latest()
            .await?
            .instance(instance)
            .await?;
        if info.paid_until == info.paid_from {
            return Ok(());
        }

        self.client.remove_instance(instance).await?;

        let mut instances = self.instances.write().unwrap();
        instances.remove(&instance);

        Ok(())
    }

    /// Start the given instance with the provided deployment.
    async fn start_instance(
        self: Arc<Self>,
        instance: Instance,
        deployment: Deployment,
        wipe_storage: bool,
    ) -> Result<()> {
        if !self.should_start_instance(instance.id, &deployment) {
            return Ok(());
        }

        // Remove any existing bundles for this instance.
        self.clone()
            .stop_instance(instance.id, wipe_storage)
            .await
            .context("failed to stop existing instance")?;

        self.set_last_instance_deployment(instance.id, &deployment);

        match self.pull_and_deploy_instance(&instance, &deployment).await {
            Ok(_) => {
                self.allow_instance_start(instance.id);
                Ok(())
            }
            Err(err) => {
                slog::error!(self.logger, "failed to deploy instance";
                    "id" => ?instance.id,
                    "err" => ?err,
                );
                self.ignore_instance_start(instance.id, err.to_string());
                Err(err)
            }
        }
    }

    fn set_last_instance_deployment(&self, instance_id: InstanceId, deployment: &Deployment) {
        let mut instances = self.instances.write().unwrap();
        let state = instances.entry(instance_id).or_default();
        state.last_deployment = Some(deployment.clone());
        state.last_error = None;
    }

    fn ignore_instance_start(&self, instance_id: InstanceId, reason: String) {
        let mut instances = self.instances.write().unwrap();
        let state = instances.entry(instance_id).or_default();
        if state.ignore_start_backoff.is_none() {
            state.ignore_start_backoff = Some(backoff::ExponentialBackoff {
                max_elapsed_time: None,
                ..Default::default()
            });
        }

        state.ignore_start_until = state
            .ignore_start_backoff
            .as_mut()
            .unwrap()
            .next_backoff()
            .and_then(|d| Instant::now().checked_add(d));

        state.last_error = Some(reason);
    }

    fn should_start_instance(&self, instance_id: InstanceId, deployment: &Deployment) -> bool {
        let mut instances = self.instances.write().unwrap();
        let state = instances.entry(instance_id).or_default();

        if let Some(last_deployment) = &state.last_deployment {
            // In case the deployment has changed, allow immediate start as the new deployment could
            // fix startup and we should make sure to process it immediately.
            if deployment != last_deployment {
                return true;
            }
        }

        if let Some(ignore_start_until) = state.ignore_start_until {
            if Instant::now() < ignore_start_until {
                return false;
            }
        }
        true
    }

    fn allow_instance_start(&self, instance_id: InstanceId) {
        let mut instances = self.instances.write().unwrap();
        if let Some(state) = instances.get_mut(&instance_id) {
            state.ignore_start_backoff = None;
            state.ignore_start_until = None;
            state.last_error = None;
        }
    }

    /// Pull the given deployment and deploy it into the given instance.
    async fn pull_and_deploy_instance(
        self: &Arc<Self>,
        instance: &Instance,
        deployment: &Deployment,
    ) -> Result<()> {
        let deployment_info = self
            .pull_and_validate_deployment(instance, deployment)
            .await?;
        self.deploy_instance(instance, deployment, deployment_info)
            .await
    }

    /// Deploy the given deployment on the given instance. Requires that the deployment has already
    /// been pulled and is available on the host under the given temporary name.
    async fn deploy_instance(
        &self,
        instance: &Instance,
        deployment: &Deployment,
        deployment_info: DeploymentInfo,
    ) -> Result<()> {
        slog::info!(self.logger, "deploying bundle";
            "id" => ?instance.id,
            "temporary_name" => &deployment_info.temporary_name,
        );

        // Check if we need to add any volumes.
        let mut volumes = BTreeMap::new();
        // TODO: Properly support multiple volumes.
        if deployment_info.volumes.len() > 1 {
            return Err(anyhow!("multiple volumes not yet supported"));
        }
        for volume_name in deployment_info.volumes {
            let mut volume_labels = labels_for_instance(instance.id);
            volume_labels.insert(LABEL_VOLUME_NAME.to_string(), "000".to_string());

            let rsp = self
                .env
                .host()
                .volume_manager()
                .volume_list(volume_manager::VolumeListRequest {
                    labels: volume_labels.clone(),
                })
                .await?;
            if rsp.volumes.is_empty() {
                // Create volume.
                let rsp = self
                    .env
                    .host()
                    .volume_manager()
                    .volume_add(volume_manager::VolumeAddRequest {
                        labels: volume_labels,
                    })
                    .await?;
                volumes.insert(volume_name, rsp.id);
            } else {
                // Use existing volume.
                volumes.insert(volume_name, rsp.volumes[0].id.clone());
            }
        }

        let mut labels = labels_for_instance(instance.id);
        labels.insert(
            LABEL_DEPLOYMENT_HASH.to_string(),
            deployment_hash(deployment),
        );

        let provider_label = ProviderLabel {
            provider: self.cfg.provider_address,
            instance: instance.id,
        };
        let provider_label = BASE64_STANDARD.encode(cbor::to_vec(provider_label));
        labels.insert(LABEL_PROVIDER.to_string(), provider_label);

        let _ = self
            .env
            .host()
            .bundle_manager()
            .bundle_add(bundle_manager::BundleAddRequest {
                temporary_name: deployment_info.temporary_name,
                manifest_hash: deployment_info.manifest_hash,
                labels,
                volumes,
            })
            .await?;

        slog::info!(self.logger, "bundle deployed";
            "id" => ?instance.id,
        );

        Ok(())
    }

    /// Pull bundle for given deployment and validate if the bundle is suitable for the instance.
    async fn pull_and_validate_deployment(
        self: &Arc<Self>,
        instance: &Instance,
        deployment: &Deployment,
    ) -> Result<DeploymentInfo> {
        slog::info!(self.logger, "pulling deployment bundle";
            "instance_id" => ?instance.id,
        );

        // TODO: Perform local caching.
        let client = oci_client::Client::new(oci_client::client::ClientConfig {
            protocol: oci_client::client::ClientProtocol::Https,
            read_timeout: Some(time::Duration::from_secs(OCI_CLIENT_READ_TIMEOUT_SECS)),
            connect_timeout: Some(time::Duration::from_secs(OCI_CLIENT_CONNECT_TIMEOUT_SECS)),
            ..Default::default()
        });
        let bundle_ref: oci_client::Reference = deployment
            .metadata
            .get(METADATA_KEY_DEPLOYMENT_ORC_REF)
            .ok_or(anyhow!("bundle location not set"))?
            .parse()
            .map_err(|_| anyhow!("bad bundle location"))?;
        // TODO: Support other authentication methods (e.g. credentials in encrypted metadata).
        let auth = oci_client::secrets::RegistryAuth::Anonymous;

        slog::info!(self.logger, "pulling OCI image manifest";
            "ref" => %bundle_ref,
        );

        // Pull manifest and config. We add a timeout to ensure we don't spend too much time
        // on fetching the OCI manifest and config as they should be small.
        let (oci_manifest, _digest, config) = time::timeout(
            time::Duration::from_secs(OCI_CLIENT_PULL_MANIFEST_TIMEOUT_SECS),
            client.pull_manifest_and_config(&bundle_ref, &auth),
        )
        .await
        .map_err(|_| anyhow!("timed out while pulling OCI manifest and config"))?
        .context("failed to pull OCI manifest and config")?;

        // Validate config and layers.
        let mut total_size: i64 = 0;
        if oci_manifest.config.media_type != OCI_TYPE_ORC_CONFIG {
            return Err(anyhow!("invalid ORC config media type"));
        }
        if oci_manifest.config.size > MAX_ORC_MANIFEST_SIZE {
            return Err(anyhow!("ORC manifest too big"));
        }
        total_size = total_size.saturating_add(oci_manifest.config.size);
        for layer in &oci_manifest.layers {
            if layer.media_type != OCI_TYPE_ORC_LAYER {
                return Err(anyhow!("invalid ORC layer media type"));
            }
            if layer.size > MAX_ORC_LAYER_SIZE {
                return Err(anyhow!("ORC layer too big"));
            }
            total_size = total_size.saturating_add(layer.size);
        }
        if total_size > MAX_ORC_TOTAL_SIZE {
            return Err(anyhow!("ORC bundle too big"));
        }

        // Parse the ORC manifest.
        slog::info!(self.logger, "got ORC manifest"; "manifest" => &config);
        let mut orc_manifest: Manifest =
            serde_json::from_str(&config).context("bad ORC manifest")?;
        orc_manifest.validate().context("invalid ORC manifest")?;

        // Verify ORC manifest hash.
        if orc_manifest.hash() != deployment.manifest_hash {
            return Err(anyhow!("invalid ORC manifest hash"));
        }

        // Ensure the ORC is for the correct runtime.
        if orc_manifest.id != self.env.runtime_id() {
            return Err(anyhow!("ORC is for the wrong network and/or runtime"));
        }

        // Validate resources in the manifest against resources of the instance.
        let mut qcow2_names = BTreeSet::new();
        let mut total_memory = 0u64;
        let mut total_cpus = 0u16;
        let mut volumes = Vec::new();
        if orc_manifest.components.len() != 1 {
            return Err(anyhow!(
                "only ORCs with exactly one component are currently supported"
            ));
        }
        for (idx, component) in orc_manifest.components.iter_mut().enumerate() {
            component.name = format!("{:03x}", idx);

            if let Some(_elf) = &component.elf {
                if component.sgx.is_none() {
                    return Err(anyhow!("missing SGX metadata in component"));
                }
            }
            if let Some(_sgx) = &component.sgx {
                if instance.resources.tee != market::types::TeeType::SGX {
                    return Err(anyhow!("ORC has incompatible TEE type"));
                }

                // TODO: Account for SGX resources, probably just threads, heap and stack size.
            }
            if let Some(tdx) = &component.tdx {
                if instance.resources.tee != market::types::TeeType::TDX {
                    return Err(anyhow!("ORC has incompatible TEE type"));
                }

                total_memory = total_memory.saturating_add(tdx.resources.memory);
                total_cpus = total_cpus.saturating_add(tdx.resources.cpus);

                // Validate artifacts against local restrictions.
                let firmware_digest = orc_manifest
                    .digests
                    .get(&tdx.firmware)
                    .ok_or(anyhow!("ORC is missing firmware digest"))?;
                self.cfg
                    .ensure_artifact_allowed("firmware", firmware_digest)?;

                if !tdx.kernel.is_empty() {
                    let kernel_digest = orc_manifest
                        .digests
                        .get(&tdx.kernel)
                        .ok_or(anyhow!("ORC is missing kernel digest"))?;
                    self.cfg.ensure_artifact_allowed("kernel", kernel_digest)?;

                    if !tdx.initrd.is_empty() {
                        let initrd_digest = orc_manifest
                            .digests
                            .get(&tdx.initrd)
                            .ok_or(anyhow!("ORC is missing initrd digest"))?;
                        self.cfg.ensure_artifact_allowed("initrd", initrd_digest)?;
                    }
                    if !tdx.stage2_image.is_empty() {
                        let stage2_digest = orc_manifest
                            .digests
                            .get(&tdx.stage2_image)
                            .ok_or(anyhow!("ORC is missing stage2 digest"))?;
                        self.cfg.ensure_artifact_allowed("stage2", stage2_digest)?;

                        qcow2_names.insert(tdx.stage2_image.clone());

                        if tdx.stage2_persist {
                            volumes.push(tdx.stage2_image.clone());
                        }
                    }
                }
            }
        }
        if total_memory > instance.resources.memory {
            return Err(anyhow!("ORC exceeds instance memory resources"));
        }
        if total_cpus > instance.resources.cpus {
            return Err(anyhow!("ORC exceeds instance vCPU resources"));
        }
        let available_storage = instance.resources.storage * 1024 * 1024;

        // Reserialize updated manifest.
        let new_orc_manifest_hash = orc_manifest.hash();
        let new_orc_manifest = serde_json::to_vec(&orc_manifest).unwrap();

        // Generate a temporary bundle name. Use instance ID to ensure temporary bundles from
        // retries don't pile up.
        let temporary_name = format!("instance-{:x}", instance.id);

        slog::info!(self.logger, "pulling OCI image layers";
            "ref" => %bundle_ref,
            "instance_id" => ?instance.id,
            "temporary_name" => &temporary_name,
        );

        let (mut reader, writer) = tokio::io::simplex(128 * 1024);

        // Start task to pull images and generate an ORC archive.
        let mgr = self.clone();
        let instance_id = instance.id;
        let layer_pull_task = tokio::spawn(async move {
            let mut zip_writer = async_zip::base::write::ZipFileWriter::with_tokio(writer);
            let mut total_storage_size = 0u64;
            let mut total_orc_size = 0usize;

            // Add updated manifest.
            let opts = async_zip::ZipEntryBuilder::new(
                manifest::MANIFEST_FILE_NAME.into(),
                async_zip::Compression::Deflate,
            );
            zip_writer
                .write_entry_whole(opts, &new_orc_manifest)
                .await?;

            // Pull and package layers.
            for layer in oci_manifest.layers {
                let name = layer
                    .annotations
                    .as_ref()
                    .ok_or(anyhow!("missing layer name"))?
                    .get(oci_client::annotations::ORG_OPENCONTAINERS_IMAGE_TITLE)
                    .ok_or(anyhow!("missing layer name"))?
                    .clone();

                slog::info!(mgr.logger, "pulling OCI layer";
                    "ref" => %bundle_ref,
                    "instance_id" => ?instance_id,
                    "layer_name" => &name,
                );

                let is_qcow2 = qcow2_names.contains(&name);
                let mut qcow2_hdr_buf = bytes::BytesMut::with_capacity(64 * 1024).writer();

                let mut hasher = Sha512_256::new();

                let opts = async_zip::ZipEntryBuilder::new(
                    name.clone().into(),
                    async_zip::Compression::Deflate,
                );
                let mut entry_writer = zip_writer.write_entry_stream(opts).await?.compat_write();
                let mut layer_stream = client.pull_blob_stream(&bundle_ref, &layer).await?;
                let mut total_orc_layer_size = 0usize;

                // When the server sends a Content-Length header, validate it first. Later on we
                // also validate the actually downloaded bytes.
                if let Some(content_length) = layer_stream.content_length {
                    if content_length > MAX_ORC_LAYER_SIZE as u64 {
                        return Err(anyhow!("ORC layer too big"));
                    }
                    if total_orc_size.saturating_add(content_length as usize)
                        > MAX_ORC_TOTAL_SIZE as usize
                    {
                        return Err(anyhow!("ORC bundle too big"));
                    }
                }

                while let Some(data) = layer_stream.next().await {
                    let data = data?;

                    // Validate layer size.
                    total_orc_layer_size = total_orc_layer_size.saturating_add(data.len());
                    if total_orc_layer_size > MAX_ORC_LAYER_SIZE as usize {
                        return Err(anyhow!("ORC layer too big"));
                    }
                    total_orc_size = total_orc_size.saturating_add(data.len());
                    if total_orc_size > MAX_ORC_TOTAL_SIZE as usize {
                        return Err(anyhow!("ORC bundle too big"));
                    }

                    // Compute layer digest to compare against ORC manifest.
                    hasher.update(&data);

                    // Collect QCOW2 header when needed.
                    if is_qcow2 && qcow2_hdr_buf.get_ref().has_remaining_mut() {
                        qcow2_hdr_buf.write_all(&data)?;
                    }

                    entry_writer.write_all(&data).await?;
                }
                entry_writer.into_inner().close().await?;

                // Validate qcow2 header.
                if is_qcow2 {
                    let qcow2_hdr_buf = qcow2_hdr_buf.into_inner();
                    let qcow2_disk_size = qcow2::parse_virtual_size(&qcow2_hdr_buf[..])?;
                    total_storage_size = total_storage_size.saturating_add(qcow2_disk_size);
                }

                // Validate layer digest.
                let layer_digest = hasher.finalize();
                let expected_digest = orc_manifest
                    .digests
                    .get(&name)
                    .ok_or(anyhow!("missing ORC layer digest"))?;
                if expected_digest != &Hash::from(layer_digest.as_slice()) {
                    return Err(anyhow!("bad ORC layer digest for '{}'", name));
                }

                // Validate storage size.
                if total_storage_size > available_storage {
                    return Err(anyhow!("ORC exceeds instance storage resources"));
                }
            }

            let writer = zip_writer.close().await?;
            writer.into_inner().shutdown().await?;

            slog::info!(mgr.logger, "all OCI layers pulled";
                "ref" => %bundle_ref,
                "instance_id" => ?instance_id,
            );

            Ok::<(), anyhow::Error>(())
        });

        // Start task to stream bundle to host.
        const CHUNK_SIZE: usize = 128 * 1024;
        let tmp_name = temporary_name.clone();
        let mgr = self.clone();
        let bundle_stream_task = tokio::spawn(async move {
            let mut create = true;
            let mut buffer = bytes::BytesMut::with_capacity(CHUNK_SIZE);
            let mut total_bundle_size = 0usize;

            loop {
                // Read from layer pull task.
                buffer.clear();
                while buffer.len() < CHUNK_SIZE {
                    let n = reader
                        .read_buf(&mut buffer)
                        .await
                        .context("failed to read bundle chunk")?;

                    if n == 0 {
                        break;
                    }
                }
                if buffer.is_empty() {
                    break;
                }

                // Ensure we never write oversized bundles to host.
                total_bundle_size = total_bundle_size.saturating_add(buffer.len());
                if total_bundle_size > MAX_ORC_TOTAL_SIZE as usize {
                    return Err(anyhow!("ORC bundle too big"));
                }

                // Write to host.
                let _ = mgr
                    .env
                    .host()
                    .bundle_manager()
                    .bundle_write(bundle_manager::BundleWriteRequest {
                        temporary_name: tmp_name.clone(), // This is suboptimal.
                        create,
                        data: buffer.to_vec(), // This is suboptimal.
                    })
                    .await
                    .context("failed to write bundle chunk to host")?;

                create = false;
            }

            Ok::<(), anyhow::Error>(())
        });

        // Wait for completion.
        async fn flatten(handle: tokio::task::JoinHandle<Result<()>>) -> Result<()> {
            handle.await?
        }

        time::timeout(
            time::Duration::from_secs(self.cfg.deploy_pull_timeout),
            async move { tokio::try_join!(flatten(layer_pull_task), flatten(bundle_stream_task)) },
        )
        .await
        .map_err(|_| anyhow!("timed out while pulling bundle"))??;

        Ok(DeploymentInfo {
            temporary_name,
            manifest_hash: new_orc_manifest_hash,
            volumes,
        })
    }

    /// Wipe storage of the given instance.
    async fn wipe_instance_storage(&self, instance_id: InstanceId) -> Result<()> {
        let _ = self
            .env
            .host()
            .volume_manager()
            .volume_remove(volume_manager::VolumeRemoveRequest {
                labels: labels_for_instance(instance_id),
            })
            .await?;

        Ok(())
    }

    /// Stop an instance, removing all bundles associated with it.
    async fn stop_instance(
        self: Arc<Self>,
        instance_id: InstanceId,
        wipe_storage: bool,
    ) -> Result<()> {
        // Wipe storage if needed.
        if wipe_storage {
            self.wipe_instance_storage(instance_id)
                .await
                .context("failed to wipe instance storage")?;
        }

        let _ = self
            .env
            .host()
            .bundle_manager()
            .bundle_remove(bundle_manager::BundleRemoveRequest {
                labels: labels_for_instance(instance_id),
            })
            .await?;

        Ok(())
    }

    /// Update instance metadata.
    async fn update_instance_metadata(
        self: Arc<Self>,
        updates: Vec<(InstanceId, InstanceUpdates)>,
    ) -> Result<()> {
        let updates = updates
            .into_iter()
            .map(|(id, update)| market::types::Update {
                id,
                node_id: update.node_id,
                deployment: update.deployment.map(Into::into),
                metadata: update.metadata,
                last_completed_cmd: update.complete_cmds,
            })
            .collect();

        self.client.update_instances(updates).await
    }

    /// Claim payment for a set of instances.
    async fn claim_payment(self: Arc<Self>, instances: Vec<InstanceId>) -> Result<()> {
        slog::info!(self.logger, "claiming payment for instances";
            "instances" => ?instances,
        );

        self.client.claim_payment(instances).await
    }
}

/// Generate labels for a given instance.
pub fn labels_for_instance(id: InstanceId) -> BTreeMap<String, String> {
    BTreeMap::from([(
        bundle_manager::LABEL_INSTANCE_ID.to_string(),
        format!("{:x}", id),
    )])
}

/// Generate deployment hash for a given deployment.
fn deployment_hash(deployment: &Deployment) -> String {
    format!(
        "{:x}",
        Hash::digest_bytes(&cbor::to_vec(deployment.clone()))
    )
}
