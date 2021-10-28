package config

import (
	"bytes"
	"fmt"
	"reflect"
	"strings"
	"sync"

	"github.com/spf13/viper"
)

var global Config

// Global returns the global configuration structure.
func Global() *Config {
	return &global
}

// Load loads the global configuration structure from viper.
func Load() error {
	return global.Load()
}

// Save saves the global configuration structure to viper.
func Save() error {
	return global.Save()
}

// ResetDefaults resets the global configuration to defaults.
func ResetDefaults() {
	global = Default
}

// Config contains the CLI configuration.
type Config struct {
	mtx sync.Mutex

	Networks Networks `mapstructure:"networks"`
	Wallets  Wallets  `mapstructure:"wallets"`
}

// Load loads the configuration structure from viper.
func (cfg *Config) Load() error {
	cfg.mtx.Lock()
	defer cfg.mtx.Unlock()
	return viper.Unmarshal(cfg)
}

// encode is needed because mapstructure cannot encode structs into maps recursively.
func encode(in interface{}) (interface{}, error) {
	const tagName = "mapstructure"

	v := reflect.ValueOf(in)
	if v.Kind() == reflect.Ptr {
		v = v.Elem()
	}

	switch v.Kind() {
	case reflect.Struct:
		// Convert structures to map[string]interface{}.
		result := make(map[string]interface{})
		t := v.Type()
		for i := 0; i < v.NumField(); i++ {
			field := t.Field(i)
			if field.PkgPath != "" {
				// Skip unexported fields.
				continue
			}

			attributes := make(map[string]bool)
			tagValue := field.Tag.Get(tagName)
			key := field.Name
			if tagValue != "" {
				attrs := strings.Split(tagValue, ",")
				key = attrs[0]
				for _, attr := range attrs[1:] {
					attributes[strings.TrimSpace(attr)] = true
				}
			}

			// Encode value.
			value, err := encode(v.Field(i).Interface())
			if err != nil {
				return nil, fmt.Errorf("failed to encode field '%s': %w", field.Name, err)
			}

			switch {
			case attributes["remain"]:
				// When remain attribute is set, merge the map.
				remaining, ok := value.(map[string]interface{})
				if !ok {
					return nil, fmt.Errorf("field '%s' with remain attribute must convert to map[string]interface{}", field.Name)
				}

				for k, v := range remaining {
					if _, exists := result[k]; exists {
						return nil, fmt.Errorf("duplicate key '%s' when processing field '%s' with remain attribute", k, field.Name)
					}
					result[k] = v
				}
			default:
				result[key] = value
			}
		}
		return result, nil
	case reflect.Map:
		// Convert maps to map[string]interface{}.
		result := make(map[string]interface{})
		iter := v.MapRange()
		for iter.Next() {
			k := iter.Key()
			v := iter.Value()

			if k.Kind() != reflect.String {
				return nil, fmt.Errorf("can only convert maps with string keys")
			}

			value, err := encode(v.Interface())
			if err != nil {
				return nil, err
			}
			result[k.Interface().(string)] = value
		}
		return result, nil
	default:
		// Pass everything else unchanged.
		return v.Interface(), nil
	}
}

// Save saves the configuration structure to viper.
func (cfg *Config) Save() error {
	cfg.mtx.Lock()
	defer cfg.mtx.Unlock()

	encCfg, err := encode(cfg)
	if err != nil {
		return err
	}
	rawCfg := encCfg.(map[string]interface{})

	// There is no other way to reset the config, so we use ReadConfig with an empty buffer.
	var buf bytes.Buffer
	viper.ReadConfig(&buf)
	// Rewrite config to use the new map.
	viper.MergeConfigMap(rawCfg)

	return viper.WriteConfig()
}

// Validate performs config validation.
func (cfg *Config) Validate() error {
	if err := cfg.Networks.Validate(); err != nil {
		return fmt.Errorf("failed to validate network configuration: %w", err)
	}
	if err := cfg.Wallets.Validate(); err != nil {
		return fmt.Errorf("failed to validate wallet configuration: %w", err)
	}
	return nil
}
