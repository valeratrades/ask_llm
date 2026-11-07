use std::{fs, path::Path};

use ask_llm::providers::claude::accounts::Accounts;
use serde_json::json;

const EXPIRED: i64 = 1_000;

/// `accounts`: name, and `.credentials.json`'s `expiresAt` and `meta.json` if logged in
fn store(accounts: &[(&str, Option<(i64, serde_json::Value)>)], current: &str) -> tempfile::TempDir {
	let root = tempfile::tempdir().unwrap();
	for (name, login) in accounts {
		let dir = root.path().join(name);
		fs::create_dir(&dir).unwrap();
		if let Some((expires_at, meta)) = login {
			fs::write(
				dir.join(".credentials.json"),
				json!({ "claudeAiOauth": { "accessToken": "sk-ant-oat01-test", "expiresAt": expires_at } }).to_string(),
			)
			.unwrap();
			fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
		}
	}
	std::os::unix::fs::symlink(current, root.path().join("current")).unwrap();
	root
}

fn meta(five_hour: f64, resets_at: &str) -> serde_json::Value {
	json!({ "email": "a@b.c", "usage": { "five_hour": { "utilization": five_hour, "resets_at": resets_at }, "seven_day": null }, "fetchedAt": 1_793_606_681_260i64 })
}

#[track_caller]
fn check_list(root: &Path, expected: &[(&str, bool, &[&str], bool)]) {
	let accounts = tokio::runtime::Runtime::new().unwrap().block_on(Accounts::at(root).list()).unwrap();
	assert_eq!(accounts.len(), expected.len());
	for (account, (name, current, label_has, disabled)) in accounts.iter().zip(expected) {
		let label = account.to_string();
		assert_eq!((account.name.as_str(), account.current, account.disabled()), (*name, *current, *disabled), "{label}");
		for part in *label_has {
			assert!(label.contains(part), "`{part}` not in `{label}`");
		}
	}
}

#[test]
fn list_reads_meta_of_an_expired_login() {
	let root = store(&[("1", None), ("2", Some((EXPIRED, meta(27.0, "2999-01-01T00:00:00Z"))))], "2");
	check_list(
		root.path(),
		&[
			("1", false, &["account 1 · not logged in"], true),
			("2", true, &["account 2 (a@b.c) · 5h 73% left, resets ", " · as of "], false),
		],
	);
}

#[test]
fn list_counts_a_passed_reset_as_whole() {
	let root = store(&[("2", Some((EXPIRED, meta(100.0, "2000-01-01T00:00:00Z"))))], "2");
	check_list(root.path(), &[("2", true, &["5h 100% left · as of "], false)]);
}

#[test]
fn list_sorts_numerically_and_skips_links() {
	let root = store(&[("10", None), ("9", None)], "10");
	std::os::unix::fs::symlink("9", root.path().join("current.123")).unwrap();
	check_list(root.path(), &[("9", false, &[], true), ("10", true, &[], true)]);
}

#[test]
fn rotate_relinks_to_a_known_account() {
	let root = store(&[("1", None), ("2", Some((EXPIRED, meta(0.0, "2999-01-01T00:00:00Z"))))], "1");
	let accounts = Accounts::at(root.path());

	assert!(accounts.rotate("2").unwrap().logged_in);
	assert_eq!(fs::read_link(root.path().join("current")).unwrap(), Path::new("2"));
	assert_eq!(accounts.current().unwrap(), fs::canonicalize(root.path().join("2")).unwrap());
	assert!(!accounts.rotate("1").unwrap().logged_in);

	for unknown in ["3", "current", "running", "..", "1/"] {
		assert!(accounts.rotate(unknown).is_err(), "{unknown}");
	}
	assert_eq!(fs::read_link(root.path().join("current")).unwrap(), Path::new("1"));
	assert_eq!(fs::read_dir(root.path()).unwrap().count(), 4, "no temporary link left behind");
}

#[test]
fn rotate_new_leaves_running_claudes_where_they_are() {
	let root = store(&[("1", None), ("2", None)], "1");
	let accounts = Accounts::at(root.path());
	accounts.rotate("1").unwrap();

	accounts.rotate_new("2").unwrap();
	assert_eq!(accounts.current().unwrap(), fs::canonicalize(root.path().join("2")).unwrap());
	assert_eq!(accounts.running().unwrap(), fs::canonicalize(root.path().join("1")).unwrap());

	accounts.rotate("2").unwrap();
	assert_eq!(accounts.running().unwrap(), fs::canonicalize(root.path().join("2")).unwrap());
}

#[test]
fn current_errors_on_a_dangling_link() {
	let root = store(&[("1", None)], "2");
	assert!(Accounts::at(root.path()).current().is_err());
}

#[test]
fn list_disables_a_spent_account_until_its_reset() {
	let root = store(&[("1", Some((EXPIRED, meta(100.0, "2999-01-01T00:00:00Z"))))], "1");
	check_list(root.path(), &[("1", true, &["5h 0% left, resets ", " · as of "], true)]);
}
