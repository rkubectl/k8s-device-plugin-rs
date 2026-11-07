//! Shared-counter conversion and pool-local reference validation.

use k8s_device_plugin_dra_core::CounterSet;
use k8s_device_plugin_dra_core::DeviceCounterConsumption;
use k8s_device_plugin_dra_core::Quantity;
use k8s_device_plugin_dra_core::is_valid_counter_name;

use super::*;

pub(super) const MAX_COUNTER_SETS_PER_SLICE: usize = 8;
const MAX_COUNTERS: usize = 32;
const MAX_CONSUMPTIONS_PER_DEVICE: usize = 2;

fn validate_counter_name(name: &str, pool_name: &str) -> kube::Result<()> {
    if !is_valid_counter_name(name) {
        return Err(invalid_pool_error(
            pool_name,
            &format!("counter name {name:?} must be a DNS label of at most 63 characters"),
        ));
    }
    Ok(())
}

fn counters_to_wire(
    counters: BTreeMap<String, Quantity>,
    pool_name: &str,
) -> kube::Result<BTreeMap<String, resourcev1::Counter>> {
    if counters.is_empty() || counters.len() > MAX_COUNTERS {
        return Err(invalid_pool_error(
            pool_name,
            "counter maps must contain 1 to 32 counters",
        ));
    }
    counters
        .into_iter()
        .map(|(name, quantity)| {
            validate_counter_name(&name, pool_name)?;
            Ok((
                name,
                resourcev1::Counter {
                    value: quantity.into(),
                },
            ))
        })
        .collect()
}

pub(super) fn counter_sets_to_wire(
    sets: Vec<CounterSet>,
    pool_name: &str,
) -> kube::Result<Vec<resourcev1::CounterSet>> {
    let mut names = BTreeSet::new();
    let mut sets = sets
        .into_iter()
        .map(|set| {
            validate_counter_name(&set.name, pool_name)?;
            if !names.insert(set.name.clone()) {
                return Err(invalid_pool_error(
                    pool_name,
                    "contains duplicate counter-set names",
                ));
            }
            Ok(resourcev1::CounterSet {
                name: set.name,
                counters: counters_to_wire(set.counters, pool_name)?,
            })
        })
        .collect::<kube::Result<Vec<_>>>()?;
    sets.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(sets)
}

pub(super) fn consumptions_to_wire(
    consumptions: Vec<DeviceCounterConsumption>,
    pool_name: &str,
) -> kube::Result<Option<Vec<resourcev1::DeviceCounterConsumption>>> {
    if consumptions.len() > MAX_CONSUMPTIONS_PER_DEVICE {
        return Err(invalid_pool_error(
            pool_name,
            "has a device with more than two counter consumptions",
        ));
    }
    let mut names = BTreeSet::new();
    let consumptions = consumptions
        .into_iter()
        .map(|consumption| {
            validate_counter_name(&consumption.counter_set, pool_name)?;
            if !names.insert(consumption.counter_set.clone()) {
                return Err(invalid_pool_error(
                    pool_name,
                    "has duplicate device counter-set references",
                ));
            }
            // v1.37 adds compatibility_groups; keep the v1.36 schema supported.
            #[allow(clippy::needless_update)]
            let wire = resourcev1::DeviceCounterConsumption {
                counter_set: consumption.counter_set,
                counters: counters_to_wire(consumption.counters, pool_name)?,
                ..default()
            };
            Ok(wire)
        })
        .collect::<kube::Result<Vec<_>>>()?;
    Ok((!consumptions.is_empty()).then_some(consumptions))
}

pub(super) fn validate_counter_references(
    devices: &[resourcev1::Device],
    sets: &[resourcev1::CounterSet],
    pool_name: &str,
) -> kube::Result<()> {
    let sets = sets
        .iter()
        .map(|set| (set.name.as_str(), &set.counters))
        .collect::<BTreeMap<_, _>>();
    for consumption in devices
        .iter()
        .flat_map(|device| device.consumes_counters.iter().flatten())
    {
        let Some(counters) = sets.get(consumption.counter_set.as_str()) else {
            return Err(invalid_pool_error(
                pool_name,
                "device references an unknown counter set",
            ));
        };
        if consumption
            .counters
            .keys()
            .any(|name| !counters.contains_key(name))
        {
            return Err(invalid_pool_error(
                pool_name,
                "device references an unknown counter",
            ));
        }
    }
    Ok(())
}
