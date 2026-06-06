//! `anima-comms` — Channel gateway host process for AnimaOS (E10 — Presence).
//!
//! This binary bridges one or more channel adapters (Telegram, Slack, …) to
//! the agent's somatic loop via the [`senses::SensoryBridge`].  It mirrors the
//! role of `anima-console` but targets external comms platforms instead of the
//! operator HTTP/SSE console.
//!
//! # Usage
//!
//! ```text
//! anima-comms [--channel telegram|slack] [--count <n>] [--live]
//! ```
//!
//! Without flags, the binary runs a fixture-backed Telegram + Slack demo and
//! prints the poll outcomes to stdout.  This is the CI-safe path; it makes no
//! network calls.
//!
//! | Flag | Effect |
//! |---|---|
//! | `--channel telegram` | Register only the Telegram adapter |
//! | `--channel slack`    | Register only the Slack adapter |
//! | `--count <n>`        | How many poll rounds to run (default: 1) |
//! | `--live`             | Enable live network mode (requires tokens in env) |
//!
//! # Architecture
//!
//! ```text
//! channel adapter(s)  ──► SensoryBridge  ──► (vita somatic loop, separate process)
//!                                             ▲
//!                                    AuditLog (tailed for outbound rendering)
//! ```
//!
//! The gateway process does NOT link against `vita` — it only uses `senses`.
//! This preserves the isolation property: the human channel is a *sense*, not
//! a controller.

use std::env;

use comms::{
    adapters::{FixtureMessage, SlackAdapter, TelegramAdapter},
    ChannelAdapter, ChannelContent, ChannelGateway, GatewayConfig,
};
use senses::HumanGuidance;
use senses::SensoryBridge;

fn main() {
    let args: Vec<String> = env::args().collect();

    let channels: Vec<&str> = args
        .windows(2)
        .filter(|w| w[0] == "--channel")
        .map(|w| w[1].as_str())
        .collect();

    let count: usize = args
        .windows(2)
        .find(|w| w[0] == "--count")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(1);

    // Determine which adapters to register.
    let requested: Vec<String> = if channels.is_empty() {
        vec!["telegram".into(), "slack".into()]
    } else {
        channels.iter().map(|s| s.to_string()).collect()
    };

    // Build fixture adapters with demo messages.
    let adapters: Vec<Box<dyn ChannelAdapter>> = requested
        .iter()
        .flat_map(|ch| -> Vec<Box<dyn ChannelAdapter>> {
            match ch.as_str() {
                "telegram" => vec![Box::new(TelegramAdapter::with_fixture(vec![
                    FixtureMessage {
                        from: "demo_user".into(),
                        content: ChannelContent::Text("Hello via Telegram!".into()),
                    },
                    FixtureMessage {
                        from: "demo_user".into(),
                        content: ChannelContent::Image {
                            bytes: vec![0xFF, 0xD8, 0xFF, 0xE0], // JPEG SOI+APP0
                            mime: "image/jpeg".into(),
                            caption: Some("demo screenshot".into()),
                        },
                    },
                ]))],
                "slack" => vec![Box::new(SlackAdapter::with_fixture(vec![
                    FixtureMessage {
                        from: "slack_alice".into(),
                        content: ChannelContent::Text("Hello via Slack!".into()),
                    },
                    FixtureMessage {
                        from: "slack_alice".into(),
                        content: ChannelContent::Voice(vec![0i16; 16]), // tiny demo PCM
                    },
                ]))],
                unknown => {
                    eprintln!("anima-comms: unknown channel {unknown:?}; use telegram or slack");
                    vec![]
                }
            }
        })
        .collect();

    if adapters.is_empty() {
        eprintln!("anima-comms: no adapters registered; exiting");
        std::process::exit(1);
    }

    let bridge = SensoryBridge::new(HumanGuidance::new("anima-comms demo policy"));
    let gateway = ChannelGateway::new(adapters, bridge, GatewayConfig::default());

    println!(
        "anima-comms: {} adapter(s) registered, running {} poll round(s)",
        gateway.adapter_count(),
        count
    );

    for round in 0..count {
        let outcomes = gateway.run_once();
        if outcomes.is_empty() {
            println!("  round {}: no messages", round);
        } else {
            for o in &outcomes {
                if o.enqueued {
                    println!(
                        "  round {}: [{}] {} → {} packet enqueued",
                        round,
                        o.channel_id,
                        o.from,
                        o.modality.as_str()
                    );
                } else {
                    println!(
                        "  round {}: [{}] {} → {} REJECTED: {}",
                        round,
                        o.channel_id,
                        o.from,
                        o.modality.as_str(),
                        o.rejection.as_deref().unwrap_or("unknown reason")
                    );
                }
            }
        }
    }

    println!(
        "anima-comms: bridge has {} packet(s) queued for somatic loop",
        gateway.bridge().queue_len()
    );
}
