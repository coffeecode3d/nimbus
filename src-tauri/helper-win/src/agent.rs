//! Orchestration for the Windows exclude agent.
//!
//! The interesting logic is "given the apps the user picked, what do we tell the
//! driver?", and that should not depend on Windows APIs to be testable. The
//! operating system is behind [`System`], so the whole flow is exercised on
//! Linux against a fake.
//!
//! The driver wants **device paths** (`\Device\HarddiskVolume3\...`), not Win32
//! paths (`C:\...`). Conversion is the system's job.

use crate::proto::{IpAddresses, ProcessEntry};

/// The parts of Windows this agent needs.
pub trait System {
    /// Convert a Win32 executable path to the device path the driver matches on.
    fn to_device_path(&self, win32_path: &str) -> Option<String>;
    /// Every process in the system, for the driver's initial registration.
    fn process_tree(&self) -> Vec<ProcessEntry>;
    /// Tunnel and physical addresses for both families.
    fn addresses(&self) -> IpAddresses;
}

/// What to hand the driver for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Images to exclude from the tunnel, in device-path form.
    pub images: Vec<String>,
    /// The process tree to register before engaging.
    pub tree: Vec<ProcessEntry>,
    pub addresses: IpAddresses,
}

/// Build a session plan from the executable paths the user selected.
///
/// Unknown paths are skipped rather than failing the session: an app that was
/// uninstalled between opening the picker and connecting should not stop the
/// VPN from coming up. Duplicates are collapsed, because the driver treats the
/// configuration as a set.
pub fn plan<S: System>(system: &S, selected: &[String]) -> Plan {
    let mut images: Vec<String> = Vec::new();
    for path in selected {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(device_path) = system.to_device_path(trimmed)
            && !images.iter().any(|existing| existing.eq_ignore_ascii_case(&device_path))
        {
            images.push(device_path);
        }
    }

    Plan {
        images,
        tree: system.process_tree(),
        addresses: system.addresses(),
    }
}

/// Whether anything would actually be excluded. An empty plan still engages the
/// driver, which is harmless but pointless; the service uses this to skip it.
pub fn is_meaningful(plan: &Plan) -> bool {
    !plan.images.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSystem {
        /// Win32 path → device path, as QueryDosDevice would report it.
        mapping: Vec<(String, String)>,
        tree: Vec<ProcessEntry>,
        addresses: IpAddresses,
    }

    impl FakeSystem {
        fn new() -> Self {
            Self {
                mapping: Vec::new(),
                tree: Vec::new(),
                addresses: IpAddresses::default(),
            }
        }
    }

    impl System for FakeSystem {
        fn to_device_path(&self, win32_path: &str) -> Option<String> {
            self.mapping
                .iter()
                .find(|(from, _)| from.eq_ignore_ascii_case(win32_path))
                .map(|(_, to)| to.clone())
        }

        fn process_tree(&self) -> Vec<ProcessEntry> {
            self.tree.clone()
        }

        fn addresses(&self) -> IpAddresses {
            self.addresses
        }
    }

    fn helium() -> (String, String) {
        (
            r"C:\Program Files\Helium\helium.exe".to_string(),
            r"\Device\HarddiskVolume3\Program Files\Helium\helium.exe".to_string(),
        )
    }

    #[test]
    fn maps_selected_paths_to_device_paths() {
        let (win32, device) = helium();
        let mut system = FakeSystem::new();
        system.mapping.push((win32.clone(), device.clone()));

        let plan = plan(&system, &[win32]);
        assert_eq!(plan.images, vec![device]);
        assert!(is_meaningful(&plan));
    }

    #[test]
    fn skips_unknown_and_empty_paths() {
        let mut system = FakeSystem::new();
        system.mapping.push(helium());

        let plan = plan(
            &system,
            &[
                r"C:\Gone\old.exe".to_string(),
                "   ".to_string(),
                r"C:\Program Files\Helium\helium.exe".to_string(),
            ],
        );
        assert_eq!(plan.images.len(), 1, "only the resolvable path is used");
    }

    #[test]
    fn collapses_duplicates_case_insensitively() {
        let (win32, device) = helium();
        let mut system = FakeSystem::new();
        system.mapping.push((win32.clone(), device.clone()));
        system.mapping.push((win32.to_uppercase(), device.to_uppercase()));

        let plan = plan(&system, &[win32.clone(), win32.to_uppercase()]);
        assert_eq!(plan.images.len(), 1, "the driver treats this as a set");
    }

    #[test]
    fn passes_the_tree_and_addresses_through() {
        let mut system = FakeSystem::new();
        system.tree.push(ProcessEntry {
            pid: 4,
            parent_pid: 0,
            image: r"\SystemRoot\system32\ntoskrnl.exe".into(),
        });
        system.addresses = IpAddresses {
            tunnel_v4: Some([10, 8, 0, 2]),
            internet_v4: Some([192, 168, 1, 20]),
            tunnel_v6: None,
            internet_v6: None,
        };

        let plan = plan(&system, &[]);
        assert_eq!(plan.tree.len(), 1);
        assert_eq!(plan.addresses.tunnel_v4, Some([10, 8, 0, 2]));
        assert!(!is_meaningful(&plan), "nothing selected means nothing to do");
    }

    #[test]
    fn an_empty_selection_yields_an_empty_image_list() {
        let system = FakeSystem::new();
        let plan = plan(&system, &[]);
        assert!(plan.images.is_empty());
        assert_eq!(plan.addresses, IpAddresses::default());
    }
}
