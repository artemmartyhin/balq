//! CLI behaviour that needs no node: exit codes, JSON shapes, config file,
//! completions, typegen.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

const PROXY: &str = "0x35825972e2ca90851b14576C531F13dA0B5d53ce";
const LAYOUT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../bal-layout/tests/fixtures/Playground.layout.json"
);

fn balq() -> Command {
    Command::cargo_bin("balq").unwrap()
}

fn json(out: &[u8]) -> Value {
    serde_json::from_slice(out).expect("valid JSON on stdout")
}

#[test]
fn status_on_fresh_archive_is_empty_and_json_shaped() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("a.redb");
    let out = balq()
        .args(["--json", "--data"])
        .arg(&data)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert!(v["head"].is_null());
    assert_eq!(v["watch"].as_array().unwrap().len(), 0);
    assert_eq!(v["bootstrap"]["pending"], 0);
    assert!(v["fileBytes"].as_u64().unwrap() > 0);
}

#[test]
fn watch_then_status_lists_it_and_rejects_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("a.redb");
    balq()
        .args(["--data"])
        .arg(&data)
        .args(["watch", PROXY, "--from", "100"])
        .assert()
        .success()
        .stdout(predicate::str::contains("watching"));
    // Same start: idempotent. Different start: refused.
    balq()
        .args(["--data"])
        .arg(&data)
        .args(["watch", PROXY, "--from", "100"])
        .assert()
        .success();
    balq()
        .args(["--data"])
        .arg(&data)
        .args(["watch", PROXY, "--from", "200"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already watched"));
    let out = balq()
        .args(["--json", "--data"])
        .arg(&data)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["watch"][0]["from"], 100);
}

#[test]
fn get_miss_is_exit_2_with_a_coded_json_error() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("a.redb");
    // Unwatched address, nothing synced: a typed reason, never a zero.
    let out = balq()
        .args(["--json", "--data"])
        .arg(&data)
        .args(["get", PROXY, "--slot", "0", "--block", "5"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let v = json(&out);
    assert_eq!(v["error"]["code"], "NotWatched");
    // Text mode says the same in words.
    balq()
        .args(["--data"])
        .arg(&data)
        .args(["get", PROXY, "--slot", "0", "--block", "5"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("NOT AVAILABLE"));
}

#[test]
fn decimal_slot_larger_than_u64_is_decimal_not_hex() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("a.redb");
    // Would previously have been re-parsed as hex; now it is a valid decimal
    // slot and the only failure is the archive being empty.
    let out = balq()
        .args(["--json", "--data"])
        .arg(&data)
        .args([
            "get",
            PROXY,
            "--slot",
            "18446744073709551616",
            "--block",
            "5",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["error"]["code"], "NotWatched");
    balq()
        .args(["--data"])
        .arg(&data)
        .args(["get", PROXY, "--slot", "0xzz", "--block", "5"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("bad hex slot"));
}

#[test]
fn config_file_supplies_data_path_and_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("balq.toml");
    let data = dir.path().join("from-config.redb");
    std::fs::write(
        &cfg,
        format!(
            "rpc = \"http://127.0.0.1:1\"\ndata = {:?}\nproof_window = 7\n",
            data.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let out = balq()
        .args(["--json", "--config"])
        .arg(&cfg)
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(json(&out)["data"]
        .as_str()
        .unwrap()
        .contains("from-config.redb"));
    assert!(data.exists(), "archive created at the configured path");
    // Unknown keys are refused, not ignored.
    std::fs::write(&cfg, "rpc = \"x\"\nbogus = 1\n").unwrap();
    balq()
        .args(["--config"])
        .arg(&cfg)
        .arg("status")
        .assert()
        .failure()
        .stderr(predicate::str::contains("bogus"));
}

#[test]
fn completions_and_typegen() {
    balq()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_balq()"));
    balq()
        .args(["typegen", LAYOUT, "--name", "PlaygroundView"])
        .assert()
        .success()
        .stdout(predicate::str::contains("export interface PlaygroundView"))
        .stdout(predicate::str::contains(
            "readonly balances: { readonly [key: string]: bigint };",
        ));
}

#[test]
fn history_with_inverted_range_is_a_coded_error() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("a.redb");
    balq()
        .args(["--data"])
        .arg(&data)
        .args(["watch", PROXY, "--from", "100"])
        .assert()
        .success();
    let out = balq()
        .args(["--json", "--data"])
        .arg(&data)
        .args(["history", PROXY, "--slot", "0", "--range", "200..100"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    assert_eq!(json(&out)["error"]["code"], "InvalidRange");
}
