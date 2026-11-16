#![cfg(feature = "relay")]
use std::net::TcpListener;
use std::process::{Child, Command as Process, Stdio};
use std::time::{Duration, Instant};
use summoner_watch::shared::{Command, Exchange, Key, Operation, Snapshot};

struct Relay(Child);
impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn real_relay_isolates_rooms_orders_team_edits_and_deduplicates_retries() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _relay = Relay(
        Process::new(env!("CARGO_BIN_EXE_summoner-watch-relay"))
            .env("SUMMONER_RELAY_BIND", format!("127.0.0.1:{port}"))
            .env("SUMMONER_RELAY_KEY", "cd".repeat(16))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let base = format!("http://127.0.0.1:{port}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(2)))
        .build()
        .into();
    let until = Instant::now() + Duration::from_secs(5);
    while agent.get(format!("{base}/health")).call().is_err() {
        assert!(Instant::now() < until, "relay failed to start");
        std::thread::sleep(Duration::from_millis(20));
    }
    let key = |champ: &str| Key {
        champion: champ.into(),
        spell: "SummonerFlash".into(),
    };
    let request = |who: &str, seq: u64, command: Option<Command>| Exchange {
        client: who.repeat(32),
        roster: vec!["Ahri".into(), "Lux".into()],
        operations: command
            .into_iter()
            .map(|command| Operation {
                sequence: seq,
                command,
            })
            .collect(),
    };
    let call = |code: &str, body: &Exchange| -> Snapshot {
        agent
            .post(format!("{base}/v1/exchange"))
            .header("Authorization", format!("Bearer {code}"))
            .header("X-Relay-Key", "cd".repeat(16))
            .send_json(body)
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap()
    };
    let code = "01".repeat(32);
    assert!(matches!(agent.post(format!("{base}/v1/exchange"))
        .header("Authorization", format!("Bearer {code}"))
        .header("X-Relay-Key", "00".repeat(16))
        .send_json(request("a", 0, None)), Err(ureq::Error::StatusCode(401))));
    let start = |champ| Command::Start {
        key: key(champ),
        remaining_ms: 300_000,
        total_ms: 300_000,
    };
    let first = call(&code, &request("a", 1, Some(start("Ahri"))));
    let original = first.timers[0].end_ms;
    assert_eq!(call(&code, &request("b", 0, None)).timers.len(), 1);
    call(&code, &request("b", 1, Some(start("Lux"))));
    // Undo reverts only the sender's own start.
    let undo = request("b", 2, Some(Command::Undo));
    let after = call(&code, &undo);
    assert_eq!(after.timers.len(), 1);
    assert_eq!(after.timers[0].key, key("Ahri"));
    assert_eq!(call(&code, &undo).timers.len(), 1); // lost-response retry
    let adjust = request(
        "b",
        3,
        Some(Command::Adjust {
            key: key("Ahri"),
            delta_ms: -5000,
        }),
    );
    let adjusted = call(&code, &adjust).timers[0].end_ms;
    assert_eq!(adjusted, original - 5000);
    assert_eq!(call(&code, &adjust).timers[0].end_ms, adjusted);
    assert!(call(&"02".repeat(32), &request("a", 0, None))
        .timers
        .is_empty());
    let mut other_roster = request("a", 0, None);
    other_roster.roster = vec!["Zed".into()];
    assert!(call(&code, &other_roster).timers.is_empty());
    assert!(matches!(
        agent
            .post(format!("{base}/v1/exchange"))
            .send_json(request("a", 0, None)),
        Err(ureq::Error::StatusCode(401))
    ));
    let mut invalid = request("a", 2, Some(start("Zed")));
    assert!(matches!(
        agent
            .post(format!("{base}/v1/exchange"))
            .header("Authorization", format!("Bearer {code}"))
            .header("X-Relay-Key", "cd".repeat(16))
            .send_json(&invalid),
        Err(ureq::Error::StatusCode(400))
    ));
    invalid.operations.clear();
    assert_eq!(call(&code, &invalid).timers.len(), 1); // invalid mutation was atomic
    call(&code, &request("a", 2, Some(Command::Reset)));
    assert!(call(&code, &request("b", 0, None)).timers.is_empty());
}
