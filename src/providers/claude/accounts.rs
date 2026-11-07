//! The device's Claude logins. See `docs/ARCHITECTURE.md#claude-accounts`.
use std::{
	fmt, fs,
	path::{Path, PathBuf},
	time::Duration,
};

use eyre::{Result, WrapErr as _, bail};
use jiff::{Timestamp, tz::TimeZone};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub struct Accounts {
	root: PathBuf,
}
impl Accounts {
	pub fn home() -> Self {
		Self::at(std::env::home_dir().expect("a user running claude has a home").join(".claude/accounts"))
	}

	pub fn at(root: impl Into<PathBuf>) -> Self {
		Self { root: root.into() }
	}

	/// where a new claude starts
	pub fn current(&self) -> Result<PathBuf> {
		self.resolve("current")
	}

	/// where every running claude follows
	pub fn running(&self) -> Result<PathBuf> {
		self.resolve("running")
	}

	/// new claudes start on `name`, and running ones move onto it
	pub fn rotate(&self, name: &str) -> Result<Rotated> {
		let rotated = self.rotate_new(name)?;
		self.relink("running", name)?;
		Ok(rotated)
	}

	/// new claudes start on `name`; running ones stay where they are
	pub fn rotate_new(&self, name: &str) -> Result<Rotated> {
		if !self.names()?.iter().any(|n| n == name) {
			bail!("no account {name} in {} (mkdir it to add one)", self.root.display());
		}
		self.relink("current", name)?;
		Ok(Rotated {
			logged_in: self.root.join(name).join(".credentials.json").exists(),
		})
	}

	fn resolve(&self, link: &str) -> Result<PathBuf> {
		let link = self.root.join(link);
		fs::canonicalize(&link).wrap_err_with(|| format!("{} names no account", link.display()))
	}

	fn relink(&self, link: &str, name: &str) -> Result<()> {
		let tmp = self.root.join(format!("{link}.{}", std::process::id()));
		std::os::unix::fs::symlink(name, &tmp)?;
		fs::rename(&tmp, self.root.join(link))?;
		Ok(())
	}

	pub async fn list(&self) -> Result<Vec<Account>> {
		let current = self.current()?;
		let mut accounts = Vec::new();
		for name in self.names()? {
			let dir = fs::canonicalize(self.root.join(&name))?;
			let login = login(&dir).await.wrap_err_with(|| format!("account {name}"))?;
			accounts.push(Account {
				current: dir == current,
				name,
				dir,
				login,
			});
		}
		Ok(accounts)
	}

	/// `current` and its temporaries are links, never dirs
	fn names(&self) -> Result<Vec<String>> {
		let mut names = Vec::new();
		for entry in fs::read_dir(&self.root).wrap_err_with(|| format!("reading {}", self.root.display()))? {
			let entry = entry?;
			if entry.file_type()?.is_dir() {
				names.push(entry.file_name().into_string().map_err(|n| eyre::eyre!("non-utf8 account name {n:?}"))?);
			}
		}
		names.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
		Ok(names)
	}
}

pub struct Rotated {
	pub logged_in: bool,
}

pub struct Account {
	pub name: String,
	/// resolved, so it compares equal to what [`Accounts::current`] returns
	pub dir: PathBuf,
	pub current: bool,
	login: Login,
}
impl Account {
	/// from the last usage snapshot; `None` until one was fetched
	pub fn email(&self) -> Option<&str> {
		match &self.login {
			Login::In { snapshot: Some(meta), .. } => Some(&meta.email),
			_ => None,
		}
	}

	/// can't be switched onto: no login, or a window last seen spent and not reset since
	pub fn disabled(&self) -> bool {
		match &self.login {
			Login::None => true,
			Login::In { snapshot, .. } => snapshot.iter().flat_map(|m| m.left(Timestamp::now())).any(|w| w.left <= 0.0 && w.reset.is_some()),
		}
	}
}

enum Login {
	None,
	In { snapshot: Option<Meta>, source: Source },
}

enum Source {
	Live,
	Cached,
	Stale(String),
}

/// `meta.json`
#[derive(Deserialize, Serialize)]
struct Meta {
	email: String,
	usage: Usage,
	#[serde(rename = "fetchedAt")]
	fetched_at: i64,
}
impl Meta {
	fn left(&self, now: Timestamp) -> Vec<Left> {
		[("5h", &self.usage.five_hour), ("week", &self.usage.seven_day)]
			.into_iter()
			.filter_map(|(label, w)| w.as_ref().map(|w| (label, w)))
			.map(|(label, w)| match w.resets_at {
				Some(reset) if reset <= now => Left { label, left: 100.0, reset: None },
				reset => Left {
					label,
					left: (100.0 - w.utilization).max(0.0),
					reset,
				},
			})
			.collect()
	}
}

#[derive(Deserialize, Serialize)]
struct Usage {
	five_hour: Option<Window>,
	seven_day: Option<Window>,
}

#[derive(Deserialize, Serialize)]
struct Window {
	utilization: f64,
	resets_at: Option<Timestamp>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credentials {
	claude_ai_oauth: Oauth,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Oauth {
	access_token: String,
	expires_at: i64,
}

#[derive(Deserialize)]
struct Profile {
	account: ProfileAccount,
}

#[derive(Deserialize)]
struct ProfileAccount {
	email: String,
}

impl fmt::Display for Account {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "account {}", self.name)?;
		let Login::In { snapshot, source } = &self.login else {
			return write!(f, " · not logged in");
		};
		if let Some(meta) = snapshot {
			write!(f, " ({})", meta.email)?;
		}
		let windows: Vec<Left> = snapshot.iter().flat_map(|m| m.left(Timestamp::now())).collect();
		if windows.is_empty() {
			write!(f, " · usage unknown")?;
		}
		for w in &windows {
			write!(f, " · {} {}% left", w.label, w.left.round())?;
		}
		if let Some(reset) = windows.iter().filter_map(|w| w.reset.map(|r| (w.left, r))).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, r)| r) {
			write!(f, ", resets {}", reset.to_zoned(TimeZone::system()).strftime("%b %-d %H:%M"))?;
		}
		match (source, snapshot) {
			(Source::Stale(e), _) => write!(f, " · stale: {e}"),
			(Source::Cached, Some(meta)) => write!(
				f,
				" · as of {}",
				Timestamp::from_millisecond(meta.fetched_at)
					.expect("written from a Timestamp")
					.to_zoned(TimeZone::system())
					.strftime("%H:%M")
			),
			_ => Ok(()),
		}
	}
}

struct Left {
	label: &'static str,
	left: f64,
	/// `None` once passed: the window is whole again
	reset: Option<Timestamp>,
}

async fn login(dir: &Path) -> Result<Login> {
	let credentials = dir.join(".credentials.json");
	if !credentials.exists() {
		return Ok(Login::None);
	}
	let oauth = serde_json::from_str::<Credentials>(&fs::read_to_string(&credentials)?)?.claude_ai_oauth;
	let file = dir.join("meta.json");
	let cached = || -> Result<Option<Meta>> {
		if !file.exists() {
			return Ok(None);
		}
		Ok(Some(serde_json::from_str(&fs::read_to_string(&file)?).wrap_err_with(|| format!("parsing {}", file.display()))?))
	};
	// live only while the access token is: refreshing it from here would race the lock of whichever claude owns the account
	if oauth.expires_at <= Timestamp::now().as_millisecond() {
		return Ok(Login::In {
			snapshot: cached()?,
			source: Source::Cached,
		});
	}
	match fetch(&oauth.access_token).await {
		Ok(meta) => {
			fs::write(&file, serde_json::to_string(&meta)?)?;
			Ok(Login::In {
				snapshot: Some(meta),
				source: Source::Live,
			})
		}
		Err(e) => Ok(Login::In {
			snapshot: cached()?,
			source: Source::Stale(format!("{e:#}")), // shown in the label, next to the snapshot it leaves standing
		}),
	}
}

async fn fetch(token: &str) -> Result<Meta> {
	let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?; // the usage endpoints take 5-7s at times
	let (usage, profile) = tokio::try_join!(get::<Usage>(&client, "usage", token), get::<Profile>(&client, "profile", token))?;
	Ok(Meta {
		email: profile.account.email,
		usage,
		fetched_at: Timestamp::now().as_millisecond(),
	})
}

async fn get<T: DeserializeOwned>(client: &reqwest::Client, endpoint: &str, token: &str) -> Result<T> {
	Ok(client
		.get(format!("https://api.anthropic.com/api/oauth/{endpoint}"))
		.bearer_auth(token)
		.header("anthropic-beta", "oauth-2025-04-20")
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?)
}
