//! `anima-console` — the operator client for AnimaOS.
//!
//! One binary, four modes, all dependency-free (`std` only):
//!
//! ```text
//! anima-console tui    [--url http://127.0.0.1:8088] [--token T]   # ANSI dashboard (default)
//! anima-console tap    [--url …] [--token T]                       # print the raw event stream
//! anima-console send   "guidance text" [--priority High] [--url …] # inject one guidance line
//! anima-console serial --device /dev/ttyS0 [--http 127.0.0.1:8088] # microVM COM1 ⇄ console bridge
//! ```
//!
//! The `tui`/`tap`/`send` modes are thin HTTP clients for the `console`
//! crate's server. The `serial` mode is the microVM Phase-0 host bridge: it
//! reads `ANIMA_TLM` NDJSON telemetry off a serial line and re-serves it over
//! the same HTTP surface, and writes operator guidance back as `ANIMA_IN`
//! lines — so the *same* dashboard works against a bare-metal kernel.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use console::{ConsoleHub, ConsoleServer, ServerConfig};
use console_proto::{json, OperatorEvent, OperatorInput, Priority};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("tui");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };

    let result = match mode {
        "tui" => run_tui(rest),
        "tap" => run_tap(rest),
        "send" => run_send(rest),
        "serial" => run_serial(rest),
        "-h" | "--help" | "help" => {
            print_help();
            Ok(())
        }
        other => {
            eprintln!("anima-console: unknown mode {other:?}\n");
            print_help();
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("anima-console: {e}");
        std::process::exit(1);
    }
}

fn print_help() {
    println!(
        "anima-console — AnimaOS operator client\n\n\
         USAGE:\n  \
         anima-console tui    [--url http://127.0.0.1:8088] [--token T]\n  \
         anima-console tap    [--url …] [--token T]\n  \
         anima-console send   \"text\" [--priority Low|Normal|High|Critical] [--url …] [--token T]\n  \
         anima-console serial --device <path|-> [--http 127.0.0.1:8088] [--token T]\n"
    );
}

// ── tiny arg helpers ─────────────────────────────────────────────────────────

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn url_or_default(args: &[String]) -> String {
    flag(args, "--url")
        .map(str::to_string)
        .or_else(|| std::env::var("ANIMA_CONSOLE_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8088".to_string())
}

fn token(args: &[String]) -> Option<String> {
    flag(args, "--token")
        .map(str::to_string)
        .or_else(|| std::env::var("ANIMA_CONSOLE_TOKEN").ok())
        .filter(|t| !t.is_empty())
}

/// Split `http://host:port/...` into `(host:port, "/path")`.
fn parse_url(url: &str) -> std::io::Result<(String, String)> {
    let stripped = url.strip_prefix("http://").unwrap_or(url);
    let (authority, path) = match stripped.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{p}")),
        None => (stripped.to_string(), "/".to_string()),
    };
    let authority = if authority.contains(':') {
        authority
    } else {
        format!("{authority}:8088")
    };
    Ok((authority, path))
}

// ── HTTP client primitives (std only) ─────────────────────────────────────────

/// Open the SSE stream and invoke `on_event` for each decoded event. Blocks.
fn stream_events(
    url: &str,
    token: Option<&str>,
    mut on_event: impl FnMut(OperatorEvent),
) -> std::io::Result<()> {
    let (authority, _) = parse_url(url)?;
    let path = match token {
        Some(t) => format!("/events?token={t}"),
        None => "/events".to_string(),
    };
    let mut stream = TcpStream::connect(&authority)?;
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: text/event-stream\r\n\r\n");
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // server closed
        }
        if let Some(payload) = line.trim_end().strip_prefix("data: ") {
            if let Some(event) = json::event_from_line(payload) {
                on_event(event);
            }
        }
    }
}

/// POST one guidance line; returns `(status_code, body)`.
fn post_guidance(
    url: &str,
    token: Option<&str>,
    input: &OperatorInput,
) -> std::io::Result<(u16, String)> {
    let (authority, _) = parse_url(url)?;
    let body = json::input_to_line(input);
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "POST /guidance HTTP/1.1\r\nHost: {authority}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = TcpStream::connect(&authority)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let code = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((code, body))
}

// ── send ───────────────────────────────────────────────────────────────────

fn run_send(args: &[String]) -> std::io::Result<()> {
    let text = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "send requires guidance text",
            )
        })?;
    let priority = flag(args, "--priority")
        .map(Priority::parse)
        .unwrap_or(Priority::Normal);
    let input = OperatorInput::new(text).with_priority(priority);
    let url = url_or_default(args);
    let (code, body) = post_guidance(&url, token(args).as_deref(), &input)?;
    println!("HTTP {code} {body}");
    if (200..300).contains(&code) {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

// ── tap ──────────────────────────────────────────────────────────────────────

fn run_tap(args: &[String]) -> std::io::Result<()> {
    let url = url_or_default(args);
    let tok = token(args);
    eprintln!("anima-console: tapping {url}/events (Ctrl-C to stop)");
    stream_events(&url, tok.as_deref(), |event| {
        println!("{}", json::event_to_line(&event));
        let _ = std::io::stdout().flush();
    })
}

// ── tui (pure-ANSI dashboard) ─────────────────────────────────────────────────

#[derive(Default)]
struct TuiState {
    vitals: [(&'static str, f32); 6],
    stress: f32,
    lifecycle: String,
    phase: String,
    agenda: u32,
    uptime: u64,
    feed: VecDeque<String>,
    connected: bool,
}

fn run_tui(args: &[String]) -> std::io::Result<()> {
    let url = url_or_default(args);
    let tok = token(args);

    let state = Arc::new(Mutex::new(TuiState {
        vitals: [
            ("thermal", 0.0),
            ("compute", 0.0),
            ("memory", 0.0),
            ("power", 1.0),
            ("budget", 1.0),
            ("attn", 0.0),
        ],
        lifecycle: "—".into(),
        phase: "—".into(),
        ..Default::default()
    }));

    // Enter alternate screen, hide cursor.
    print!("\x1b[?1049h\x1b[?25l");
    let _ = std::io::stdout().flush();

    // Stdin reader: each line is guidance; `/q` quits, `!`/`!!` raise priority.
    {
        let url = url.clone();
        let tok = tok.clone();
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines().map_while(Result::ok) {
                let line = line.trim().to_string();
                if line == "/q" || line == "/quit" {
                    print!("\x1b[?25h\x1b[?1049l");
                    let _ = std::io::stdout().flush();
                    std::process::exit(0);
                }
                if line.is_empty() {
                    continue;
                }
                let (priority, text) = parse_tui_input(&line);
                let input = OperatorInput::new(text).with_priority(priority);
                match post_guidance(&url, tok.as_deref(), &input) {
                    Ok((code, _)) if (200..300).contains(&code) => {
                        push_feed(&state, format!("» sent [{}] guidance", priority.as_str()));
                    }
                    Ok((code, body)) => push_feed(&state, format!("» rejected ({code}): {body}")),
                    Err(e) => push_feed(&state, format!("» send error: {e}")),
                }
                render(&state);
            }
        });
    }

    // Reconnecting event loop on the main thread.
    loop {
        {
            let mut s = state.lock().unwrap();
            s.connected = false;
        }
        render(&state);
        let state_cb = Arc::clone(&state);
        let _ = stream_events(&url, tok.as_deref(), move |event| {
            apply_event(&state_cb, event);
            render(&state_cb);
        });
        // Disconnected — pause then retry.
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn parse_tui_input(line: &str) -> (Priority, String) {
    if let Some(rest) = line.strip_prefix("!!") {
        (Priority::Critical, rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix('!') {
        (Priority::High, rest.trim().to_string())
    } else {
        (Priority::Normal, line.to_string())
    }
}

fn push_feed(state: &Arc<Mutex<TuiState>>, line: String) {
    let mut s = state.lock().unwrap();
    s.feed.push_front(line);
    while s.feed.len() > 200 {
        s.feed.pop_back();
    }
}

fn apply_event(state: &Arc<Mutex<TuiState>>, event: OperatorEvent) {
    let mut s = state.lock().unwrap();
    s.connected = true;
    match event {
        OperatorEvent::Vitals {
            thermal_load,
            compute_pressure,
            memory_pressure,
            power_budget,
            financial_budget,
            attention_demand,
            aggregate_stress,
        } => {
            s.vitals = [
                ("thermal", thermal_load),
                ("compute", compute_pressure),
                ("memory", memory_pressure),
                ("power", power_budget),
                ("budget", financial_budget),
                ("attn", attention_demand),
            ];
            s.stress = aggregate_stress;
        }
        OperatorEvent::State {
            lifecycle,
            sleep_phase,
            agenda_depth,
        } => {
            s.lifecycle = lifecycle;
            s.phase = sleep_phase.unwrap_or_else(|| "—".into());
            s.agenda = agenda_depth;
        }
        OperatorEvent::Gate {
            invoke,
            cost_class,
            value_score,
            threshold,
            reasoning,
            ..
        } => {
            let verdict = if invoke { "INVOKE" } else { "block " };
            s.feed.push_front(format!(
                "gate  {verdict} {} v{value_score:.2}/t{threshold:.2} — {reasoning}",
                cost_class.unwrap_or_default()
            ));
        }
        OperatorEvent::TaskStarted { task_id, prompt } => {
            s.feed.push_front(format!("task→ #{task_id} {prompt}"));
        }
        OperatorEvent::AgentMessage {
            task_id,
            tokens,
            text,
        } => {
            s.feed
                .push_front(format!("agent #{task_id} ({tokens}t) {text}"));
        }
        OperatorEvent::Audit { kind, detail } => {
            s.feed.push_front(format!("{kind}: {detail}"));
        }
        OperatorEvent::Heartbeat { uptime_secs } => s.uptime = uptime_secs,
    }
    while s.feed.len() > 200 {
        s.feed.pop_back();
    }
}

fn bar(v: f32, width: usize) -> String {
    let filled = ((v.clamp(0.0, 1.0)) * width as f32).round() as usize;
    let color = if v > 0.85 {
        "\x1b[31m"
    } else if v > 0.6 {
        "\x1b[33m"
    } else {
        "\x1b[36m"
    };
    format!(
        "{color}{}\x1b[90m{}\x1b[0m",
        "█".repeat(filled),
        "░".repeat(width - filled)
    )
}

fn render(state: &Arc<Mutex<TuiState>>) {
    let s = state.lock().unwrap();
    let mut out = String::new();
    out.push_str("\x1b[2J\x1b[H"); // clear + home
    let dot = if s.connected {
        "\x1b[32m●\x1b[0m"
    } else {
        "\x1b[31m●\x1b[0m"
    };
    out.push_str(&format!(
        "\x1b[1mANIMA\x1b[36mOS\x1b[0m operator console  {dot}  uptime {}s   \x1b[90m(type guidance + Enter · ! = High · !! = Critical · /q = quit)\x1b[0m\n",
        s.uptime
    ));
    out.push_str("\x1b[90m──────────────────────────────────────────────────────────────────────────\x1b[0m\n");

    out.push_str(&format!(
        "\x1b[1mVITALS\x1b[0m   stress \x1b[1m{:.2}\x1b[0m\n",
        s.stress
    ));
    for (label, v) in s.vitals {
        out.push_str(&format!("  {label:<8} {} {:.2}\n", bar(v, 28), v));
    }
    out.push_str(&format!(
        "\n\x1b[1mLIFECYCLE\x1b[0m  state \x1b[36m{}\x1b[0m   phase {}   agenda {}\n",
        s.lifecycle, s.phase, s.agenda
    ));
    out.push_str("\x1b[90m──────────────────────────────────────────────────────────────────────────\x1b[0m\n");
    out.push_str("\x1b[1mEVENT STREAM\x1b[0m\n");
    for line in s.feed.iter().take(20) {
        let colored = if line.starts_with("agent") {
            format!("\x1b[35m{line}\x1b[0m")
        } else if line.starts_with("gate") {
            format!("\x1b[36m{line}\x1b[0m")
        } else if line.starts_with("DefenceVeto") || line.starts_with("» rejected") {
            format!("\x1b[31m{line}\x1b[0m")
        } else if line.starts_with("task") {
            format!("\x1b[32m{line}\x1b[0m")
        } else {
            format!("\x1b[33m{line}\x1b[0m")
        };
        // Trim very long lines so the screen stays stable.
        let trimmed: String = colored.chars().take(160).collect();
        out.push_str(&format!("  {trimmed}\n"));
    }
    print!("{out}");
    let _ = std::io::stdout().flush();
}

// ── serial bridge (microVM COM1 ⇄ console) ────────────────────────────────────

fn run_serial(args: &[String]) -> std::io::Result<()> {
    let device = flag(args, "--device").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "serial mode requires --device <path|->",
        )
    })?;
    let http = flag(args, "--http").unwrap_or("127.0.0.1:8088").to_string();

    // Open the serial line for reading (and writing guidance back).
    let (reader, writer): (Box<dyn Read + Send>, Box<dyn Write + Send>) = if device == "-" {
        (Box::new(std::io::stdin()), Box::new(std::io::stdout()))
    } else {
        let r = std::fs::OpenOptions::new().read(true).open(device)?;
        let w = std::fs::OpenOptions::new().write(true).open(device)?;
        (Box::new(r), Box::new(w))
    };

    // Shared hub + sensory bridge; the HTTP server lets the dashboard/TUI attach.
    let hub = Arc::new(ConsoleHub::new());
    let bridge = senses::SensoryBridge::new(senses::HumanGuidance::new("serial-bridge"));
    let server = ConsoleServer::new(
        Arc::clone(&hub),
        bridge.clone(),
        ServerConfig {
            addr: http.clone(),
            token: token(args),
        },
    );
    let (addr, _h) = server.spawn()?;
    eprintln!("anima-console: serial bridge on {device} ⇄ http://{addr}");

    // Drain operator guidance → write `ANIMA_IN {json}` lines to the serial line.
    {
        let bridge = bridge.clone();
        let writer = Arc::new(Mutex::new(writer));
        std::thread::spawn(move || loop {
            while let Some(pkt) = bridge.next_prioritized_packet() {
                if let senses::SensoryPacket::Text(text) = pkt.packet {
                    let input = OperatorInput::new(text).with_priority(match pkt.priority {
                        senses::SensoryPriority::Low => Priority::Low,
                        senses::SensoryPriority::Normal => Priority::Normal,
                        senses::SensoryPriority::High => Priority::High,
                        senses::SensoryPriority::Critical => Priority::Critical,
                    });
                    let line = format!(
                        "{}{}\n",
                        console_proto::INPUT_PREFIX,
                        json::input_to_line(&input)
                    );
                    let mut w = writer.lock().unwrap();
                    let _ = w.write_all(line.as_bytes());
                    let _ = w.flush();
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        });
    }

    // Read serial telemetry → publish `ANIMA_TLM` lines as OperatorEvents.
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            eprintln!("anima-console: serial stream closed");
            return Ok(());
        }
        let trimmed = line.trim_end();
        if let Some(payload) = trimmed.strip_prefix(console_proto::TELEMETRY_PREFIX) {
            if let Some(event) = json::event_from_line(payload) {
                hub.publish(event);
            }
        }
        // Lines without the telemetry prefix (boot markers, panics) are echoed
        // to stderr so the operator still sees the raw serial console.
        else if !trimmed.is_empty() {
            eprintln!("[serial] {trimmed}");
        }
    }
}
