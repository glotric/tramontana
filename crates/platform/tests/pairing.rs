//! SP0-C spike (`docs/SHARED_PROJECTS_PLAN.md`): pairing a device with a host.
//!
//! No passwords and no passkeys. The owner makes a one-time invite for a
//! person; the person's desktop redeems it once and receives a device token;
//! every later request carries that token. The host keeps only hashes, so
//! its database never holds anything that would let a reader sign in.
//!
//! Throwaway: the host's side lives in this file. The question is whether
//! this shape is enough, not what its production module looks like.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests assert on known-good values; a panic is the failure report"
)]

use std::collections::HashMap;

use sha2::{Digest, Sha256};

/// Seconds an invite stays redeemable.
const INVITE_LIFETIME: u64 = 15 * 60;

/// A secret the host can recognize without storing: 244 random bits.
///
/// uuid's v4 draws from the operating system's generator (`getrandom`), and
/// `uuid` is already a dependency, so no new crate is needed for randomness.
fn random_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// What the host stores in place of a secret. A plain SHA-256 is enough:
/// the secret is random, not a person's password, so there is nothing for a
/// slow hash to protect against guessing.
fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[derive(Debug)]
struct Invite {
    person: String,
    expires_at: u64,
}

#[derive(Debug)]
struct Device {
    id: u64,
    person: String,
    name: String,
}

#[derive(Debug, PartialEq)]
enum PairError {
    /// Unknown, used, or expired. One answer, so a guesser learns nothing.
    InvalidInvite,
}

#[derive(Debug, Default)]
struct Host {
    /// The host's TLS certificate fingerprint, carried in every invite so
    /// the guest can refuse a host that is not the one the owner meant.
    fingerprint: String,
    invites: HashMap<[u8; 32], Invite>,
    devices: HashMap<[u8; 32], Device>,
    next_device: u64,
}

impl Host {
    /// The owner invites `person`. The link is what the owner sends them.
    fn invite(&mut self, person: &str, now: u64) -> String {
        let code = random_secret();
        self.invites.insert(
            digest(&code),
            Invite {
                person: person.to_owned(),
                expires_at: now + INVITE_LIFETIME,
            },
        );
        format!(
            "tramontana://pair?host=nas.local:7443&fingerprint={}&code={code}",
            self.fingerprint
        )
    }

    /// A device redeems an invite once and gets its token.
    fn redeem(
        &mut self,
        code: &str,
        device_name: &str,
        now: u64,
    ) -> Result<(u64, String), PairError> {
        // Removed whatever happens next: a code is spent by its first use.
        let invite = self
            .invites
            .remove(&digest(code))
            .ok_or(PairError::InvalidInvite)?;
        if now >= invite.expires_at {
            return Err(PairError::InvalidInvite);
        }
        let token = random_secret();
        self.next_device += 1;
        self.devices.insert(
            digest(&token),
            Device {
                id: self.next_device,
                person: invite.person,
                name: device_name.to_owned(),
            },
        );
        Ok((self.next_device, token))
    }

    /// Who is asking, from the token on a request. `None` fails closed.
    fn authenticate(&self, token: &str) -> Option<&str> {
        self.devices
            .get(&digest(token))
            .map(|device| device.person.as_str())
    }

    /// What the owner sees when choosing a device to revoke.
    fn devices_of(&self, person: &str) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .devices
            .values()
            .filter(|device| device.person == person)
            .map(|device| device.name.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    fn revoke_device(&mut self, id: u64) {
        self.devices.retain(|_, device| device.id != id);
    }

    /// Removing a person ends every device they paired, at once.
    fn remove_person(&mut self, person: &str) {
        self.devices.retain(|_, device| device.person != person);
        self.invites.retain(|_, invite| invite.person != person);
    }
}

fn code_of(link: &str) -> &str {
    link.rsplit_once("code=").unwrap().1
}

fn host() -> Host {
    Host {
        fingerprint: "sha256-AB12".to_owned(),
        ..Host::default()
    }
}

#[test]
fn an_invite_pairs_one_device_as_the_invited_person() {
    let mut host = host();
    let link = host.invite("ana", 0);
    assert!(link.contains("fingerprint=sha256-AB12"));

    let (_, token) = host.redeem(code_of(&link), "Ana's MacBook", 10).unwrap();
    assert_eq!(host.authenticate(&token), Some("ana"));
    assert_eq!(host.authenticate("a-guessed-token"), None);
}

#[test]
fn an_invite_works_once_and_not_after_it_expires() {
    let mut host = host();
    let link = host.invite("ana", 0);
    host.redeem(code_of(&link), "first", 10).unwrap();
    assert_eq!(
        host.redeem(code_of(&link), "second", 11),
        Err(PairError::InvalidInvite)
    );

    let late = host.invite("bo", 0);
    assert_eq!(
        host.redeem(code_of(&late), "too late", INVITE_LIFETIME),
        Err(PairError::InvalidInvite)
    );
    // A failed redemption still spends the code.
    assert_eq!(
        host.redeem(code_of(&late), "again", 1),
        Err(PairError::InvalidInvite)
    );
}

#[test]
fn the_host_stores_no_code_or_token_it_could_leak() {
    let mut host = host();
    let link = host.invite("ana", 0);
    let pending = host.invite("bo", 0);
    let (_, token) = host.redeem(code_of(&link), "mac", 1).unwrap();

    let stored = format!("{host:?}");
    assert!(!stored.contains(&token));
    assert!(!stored.contains(code_of(&pending)));
}

#[test]
fn revoking_a_device_ends_only_that_device_and_removing_a_person_ends_all() {
    fn pair(host: &mut Host, person: &str, device: &str) -> (u64, String) {
        let link = host.invite(person, 0);
        host.redeem(code_of(&link), device, 1).unwrap()
    }
    let mut host = host();
    let (laptop, laptop_token) = pair(&mut host, "ana", "laptop");
    let (_, desktop_token) = pair(&mut host, "ana", "desktop");
    let (_, bo_token) = pair(&mut host, "bo", "bo's");

    assert_eq!(host.devices_of("ana"), ["desktop", "laptop"]);
    host.revoke_device(laptop);
    assert_eq!(host.devices_of("ana"), ["desktop"]);
    assert_eq!(host.authenticate(&laptop_token), None);
    assert_eq!(host.authenticate(&desktop_token), Some("ana"));

    let unused = host.invite("ana", 2);
    host.remove_person("ana");
    assert_eq!(host.authenticate(&desktop_token), None);
    assert_eq!(
        host.redeem(code_of(&unused), "after removal", 3),
        Err(PairError::InvalidInvite)
    );
    assert_eq!(host.authenticate(&bo_token), Some("bo"));
}

#[test]
fn secrets_are_long_and_do_not_repeat() {
    let secrets: std::collections::HashSet<String> = (0..1_000).map(|_| random_secret()).collect();
    assert_eq!(secrets.len(), 1_000);
    assert!(secrets.iter().all(|secret| secret.len() == 64));
}
