//! WASM ABI supported by the contracts module.
use oasis_contract_sdk_types::{message::Reply, ExecutionOk};
use oasis_runtime_sdk::{
    context::Context,
    types::{address::Address, token},
};

use super::{types, Error, Parameters};

pub mod gas;
pub mod oasis;

/// Trait for any WASM ABI to implement.
pub trait Abi<C: Context> {
    /// Validate that the given WASM module conforms to the ABI.
    fn validate(&self, module: &mut walrus::Module) -> Result<(), Error>;

    /// Link required functions into the WASM module instance.
    fn link(
        &self,
        instance: &mut wasm3::Instance<'_, '_, ExecutionContext<'_, C>>,
    ) -> Result<(), Error>;

    /// Set the gas limit for any following executions.
    ///
    /// The specified gas limit should be in regular SDK gas units, not in WASM gas units. The ABI
    /// should perform any necessary conversions if required.
    fn set_gas_limit(
        &self,
        instance: &mut wasm3::Instance<'_, '_, ExecutionContext<'_, C>>,
        gas_limit: u64,
    ) -> Result<(), Error>;

    /// Instantiate a contract.
    fn instantiate<'ctx>(
        &self,
        ctx: &mut ExecutionContext<'ctx, C>,
        instance: &wasm3::Instance<'_, '_, ExecutionContext<'ctx, C>>,
        request: &[u8],
        deposited_tokens: &[token::BaseUnits],
    ) -> ExecutionResult;

    /// Call a contract.
    fn call<'ctx>(
        &self,
        ctx: &mut ExecutionContext<'ctx, C>,
        instance: &wasm3::Instance<'_, '_, ExecutionContext<'ctx, C>>,
        request: &[u8],
        deposited_tokens: &[token::BaseUnits],
    ) -> ExecutionResult;

    /// Invoke the contract's reply handler.
    fn handle_reply<'ctx>(
        &self,
        ctx: &mut ExecutionContext<'ctx, C>,
        instance: &wasm3::Instance<'_, '_, ExecutionContext<'ctx, C>>,
        reply: Reply,
    ) -> ExecutionResult;

    /// Invoke the contract's pre-upgrade handler.
    fn pre_upgrade<'ctx>(
        &self,
        ctx: &mut ExecutionContext<'ctx, C>,
        instance: &wasm3::Instance<'_, '_, ExecutionContext<'ctx, C>>,
        request: &[u8],
        deposited_tokens: &[token::BaseUnits],
    ) -> ExecutionResult;

    /// Invoke the contract's post-upgrade handler.
    fn post_upgrade<'ctx>(
        &self,
        ctx: &mut ExecutionContext<'ctx, C>,
        instance: &wasm3::Instance<'_, '_, ExecutionContext<'ctx, C>>,
        request: &[u8],
        deposited_tokens: &[token::BaseUnits],
    ) -> ExecutionResult;

    /// Query a contract.
    fn query<'ctx>(
        &self,
        ctx: &mut ExecutionContext<'ctx, C>,
        instance: &wasm3::Instance<'_, '_, ExecutionContext<'ctx, C>>,
        request: &[u8],
    ) -> ExecutionResult;
}

/// Execution context.
pub struct ExecutionContext<'ctx, C: Context> {
    /// Transaction context.
    pub tx_context: &'ctx mut C,
    /// Contracts module parameters.
    pub params: &'ctx Parameters,

    /// Contract instance information.
    pub instance_info: &'ctx types::Instance,
    /// Gas limit for this contract execution.
    pub gas_limit: u64,

    /// Address of the caller.
    pub caller_address: Address,
}

/// Result of an execution that contains additional metadata like gas used.
#[must_use]
pub struct ExecutionResult {
    /// Actual execution result.
    pub inner: Result<ExecutionOk, Error>,
    /// Amount of gas used by the execution.
    pub gas_used: u64,
}
