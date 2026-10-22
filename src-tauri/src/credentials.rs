//! Account credentials in the operating system's credential store.
//!
//! Eddie's jsoninout engine needs the AirVPN password for every `act=connect`
//! authorization call, but the profile must not contain it. Keeping it in the
//! OS keyring (Secret Service on Linux, Credential Manager on Windows, Keychain
//! on macOS) is what lets Nimbus stay signed in across launches without writing
//! a plaintext secret to disk.

const SERVICE: &str = "nimbus.airvpn";

fn entry(login: &str) -> Result<keyring::Entry, String> {
    let login = login.trim();
    if login.is_empty() {
        return Err("AirVPN login is empty.".into());
    }
    keyring::Entry::new(SERVICE, login)
        .map_err(|e| format!("System keyring is unavailable: {e}"))
}

#[tauri::command]
pub fn credentials_save(login: String, password: String) -> Result<(), String> {
    if password.is_empty() {
        return Err("Password is empty.".into());
    }
    entry(&login)?
        .set_password(&password)
        .map_err(|e| format!("Could not save credentials in the system keyring: {e}"))
}

#[tauri::command]
pub fn credentials_load(login: String) -> Result<Option<String>, String> {
    if login.trim().is_empty() {
        return Ok(None);
    }
    match entry(&login)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Could not read credentials from the system keyring: {e}")),
    }
}

#[tauri::command]
pub fn credentials_delete(login: String) -> Result<(), String> {
    if login.trim().is_empty() {
        return Ok(());
    }
    match entry(&login)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Could not remove credentials from the system keyring: {e}")),
    }
}

#[cfg(test)]
mod tests {
    /// Exercises the same platform store the app uses, with a throwaway service
    /// name, so a broken Secret Service setup is caught before sign-in.
    #[test]
    fn keyring_round_trip() {
        let entry = keyring::Entry::new("nimbus.airvpn.test", "round-trip")
            .expect("open test keyring entry");
        entry.set_password("nimbus-test-secret").expect("save secret");
        assert_eq!(entry.get_password().expect("read secret"), "nimbus-test-secret");
        entry.delete_credential().expect("delete secret");
        assert!(matches!(
            entry.get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }
}
