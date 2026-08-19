use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::thread;

use anyhow::{Context, Result, bail};
use iceoryx2::port::publisher::Publisher;
use iceoryx2::prelude::*;
use iceoryx2::service::ipc;
use tracing::{info, warn};

use crate::config::SourceConfig;

pub const RELOAD_NOTIFY_PAYLOAD: usize = 512;
pub const RELOAD_NOTIFY_MAX_PUBLISHERS: usize = 4;
pub const RELOAD_NOTIFY_MAX_SUBSCRIBERS: usize = 8;
pub const RELOAD_NOTIFY_HISTORY_SIZE: usize = 32;
pub const RELOAD_NOTIFY_SUBSCRIBER_BUFFER: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadNotify {
    pub strategy_name: String,
    pub updated_at_us: i64,
}

#[derive(Clone)]
pub struct ReloadNotifyHub {
    tx: Sender<NotifyCommand>,
}

struct NotifyCommand {
    source_id: String,
    service_name: String,
    notify: ReloadNotify,
}

impl ReloadNotifyHub {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<NotifyCommand>();
        thread::Builder::new()
            .name("cta-reload-notify".to_string())
            .spawn(move || {
                let mut publishers = HashMap::<String, Publisher<ipc::Service, [u8; RELOAD_NOTIFY_PAYLOAD], ()>>::new();
                while let Ok(command) = rx.recv() {
                    if let Err(error) = publish_command(&mut publishers, &command) {
                        warn!(
                            source_id = %command.source_id,
                            service_name = %command.service_name,
                            strategy_name = %command.notify.strategy_name,
                            updated_at_us = command.notify.updated_at_us,
                            error = %error,
                            "reload notify failed after Redis write; 30s Redis poll remains the fallback"
                        );
                    }
                }
            })
            .expect("failed to spawn reload notify thread");
        Self { tx }
    }

    pub fn notify(&self, source: &SourceConfig, strategy_name: &str, updated_at_us: i64) {
        let Some(service_name) = source.reload_notify_service_name() else {
            warn!(
                source_id = %source.id,
                strategy_name,
                "reload notify skipped: source has no iceoryx namespace"
            );
            return;
        };
        if updated_at_us <= 0 {
            warn!(
                source_id = %source.id,
                strategy_name,
                "reload notify skipped: published Redis value has no updated_at_us"
            );
            return;
        }
        if let Err(error) = self.tx.send(NotifyCommand {
            source_id: source.id.clone(),
            service_name,
            notify: ReloadNotify {
                strategy_name: strategy_name.to_string(),
                updated_at_us,
            },
        }) {
            warn!(
                source_id = %source.id,
                strategy_name,
                error = %error,
                "reload notify channel closed; 30s Redis poll remains the fallback"
            );
        }
    }
}

fn publish_command(
    publishers: &mut HashMap<String, Publisher<ipc::Service, [u8; RELOAD_NOTIFY_PAYLOAD], ()>>,
    command: &NotifyCommand,
) -> Result<()> {
    if !publishers.contains_key(&command.service_name) {
        let publisher = create_publisher(&command.source_id, &command.service_name)?;
        publishers.insert(command.service_name.clone(), publisher);
    }
    let publisher = publishers
        .get(&command.service_name)
        .context("reload notify publisher missing after insert")?;
    let bytes = encode_reload_notify(&command.notify)?;
    let mut sample = publisher.loan_uninit()?;
    unsafe {
        let payload = sample.payload_mut().as_mut_ptr().cast::<u8>();
        let out = std::slice::from_raw_parts_mut(payload, RELOAD_NOTIFY_PAYLOAD);
        out.copy_from_slice(&bytes);
        sample.assume_init().send()?;
    }
    info!(
        source_id = %command.source_id,
        service_name = %command.service_name,
        strategy_name = %command.notify.strategy_name,
        updated_at_us = command.notify.updated_at_us,
        "reload notify published after confirmed Redis write"
    );
    Ok(())
}

fn create_publisher(
    source_id: &str,
    service_name: &str,
) -> Result<Publisher<ipc::Service, [u8; RELOAD_NOTIFY_PAYLOAD], ()>> {
    let node_name = format!(
        "cta_web_reload_{}",
        source_id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect::<String>()
    );
    let node = NodeBuilder::new()
        .name(&NodeName::new(&node_name)?)
        .create::<ipc::Service>()
        .with_context(|| format!("failed to create iceoryx node {node_name}"))?;
    let service = node
        .service_builder(&ServiceName::new(service_name)?)
        .publish_subscribe::<[u8; RELOAD_NOTIFY_PAYLOAD]>()
        .max_publishers(RELOAD_NOTIFY_MAX_PUBLISHERS)
        .max_subscribers(RELOAD_NOTIFY_MAX_SUBSCRIBERS)
        .history_size(RELOAD_NOTIFY_HISTORY_SIZE)
        .subscriber_max_buffer_size(RELOAD_NOTIFY_SUBSCRIBER_BUFFER)
        .open_or_create()
        .with_context(|| format!("failed to open reload notify service {service_name}"))?;
    service
        .publisher_builder()
        .create()
        .with_context(|| format!("failed to create reload notify publisher {service_name}"))
}

pub fn encode_reload_notify(notify: &ReloadNotify) -> Result<[u8; RELOAD_NOTIFY_PAYLOAD]> {
    if notify.updated_at_us <= 0 {
        bail!("updated_at_us must be positive");
    }
    let name = notify.strategy_name.as_bytes();
    if name.len() > 255 {
        bail!("strategy_name exceeds reload notify payload");
    }
    let mut bytes = [0u8; RELOAD_NOTIFY_PAYLOAD];
    bytes[..8].copy_from_slice(&notify.updated_at_us.to_le_bytes());
    bytes[8] = name.len() as u8;
    bytes[9..9 + name.len()].copy_from_slice(name);
    Ok(bytes)
}

pub fn decode_reload_notify(bytes: &[u8; RELOAD_NOTIFY_PAYLOAD]) -> Option<ReloadNotify> {
    let updated_at_us = i64::from_le_bytes(bytes[0..8].try_into().ok()?);
    if updated_at_us <= 0 {
        return None;
    }
    let name_len = usize::from(bytes[8]);
    let name_bytes = bytes.get(9..9 + name_len)?;
    let strategy_name = std::str::from_utf8(name_bytes).ok()?.to_string();
    Some(ReloadNotify {
        strategy_name,
        updated_at_us,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_notify_round_trips_strategy_and_version() {
        let notify = ReloadNotify {
            strategy_name: "CTA_SK_C4V6PosT1_LXY_filter_Position".to_string(),
            updated_at_us: 1_725_000_000_000_001,
        };
        let encoded = encode_reload_notify(&notify).unwrap();
        assert_eq!(decode_reload_notify(&encoded), Some(notify));
    }

    #[test]
    fn reload_notify_rejects_unconfirmed_version() {
        assert!(
            encode_reload_notify(&ReloadNotify {
                strategy_name: "CTA_A".to_string(),
                updated_at_us: 0,
            })
            .is_err()
        );
        let mut bytes = encode_reload_notify(&ReloadNotify {
            strategy_name: "CTA_A".to_string(),
            updated_at_us: 12,
        })
        .unwrap();
        bytes[..8].copy_from_slice(&0i64.to_le_bytes());
        assert_eq!(decode_reload_notify(&bytes), None);
    }
}
