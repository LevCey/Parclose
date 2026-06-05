use odra::prelude::*;

#[odra::odra_error]
pub enum Error {
    NotAdmin = 0,
    WindowNotFound = 1,
    WindowAlreadyClosed = 2,
}

/// Metadata stored for each crossing window.
#[odra::odra_type]
pub struct WindowInfo {
    pub is_open: bool,
    pub opened_at: u64,
    pub closed_at: u64,
}

/// Registry of crossing windows and the published uniform-price clearing rule.
///
/// Window status is the authority gate used by SealedOrderBook (accept only while open)
/// and CrossingEngine (settle only once the window is closed).
#[odra::module(errors = Error)]
pub struct WindowRegistry {
    admin: Var<Address>,
    // next_window_id is the ID that will be assigned to the next opened window.
    next_wid: Var<u64>,
    // Most recently opened window; 0 = none yet.
    active_wid: Var<u64>,
    windows: Mapping<u64, WindowInfo>,
    published_rule: Var<String>,
    // rule_ver and rule_history track the full published-rule history so CrossingEngine
    // can confirm the attestation's rule_version matches the current on-chain version.
    rule_ver: Var<u32>,
    rule_history: Mapping<u32, String>,
}

#[odra::module]
impl WindowRegistry {
    /// Deploys the registry, records the deployer as admin, and publishes the initial rule.
    pub fn init(&mut self, initial_rule: String) {
        let deployer = self.env().caller();
        self.admin.set(deployer);
        self.next_wid.set(1u64);
        self.rule_ver.set(1u32);
        self.published_rule.set(initial_rule.clone());
        self.rule_history.set(&1u32, initial_rule);
    }

    /// Opens a new crossing window. Admin only. Returns the new window_id.
    pub fn open_window(&mut self) -> u64 {
        self.assert_admin();
        let wid = self.next_wid.get_or_default();
        let now = self.env().get_block_time();
        self.windows.set(
            &wid,
            WindowInfo {
                is_open: true,
                opened_at: now,
                closed_at: 0,
            },
        );
        self.active_wid.set(wid);
        self.next_wid.set(wid + 1);
        wid
    }

    /// Closes an open crossing window. Admin only.
    pub fn close_window(&mut self, window_id: u64) {
        self.assert_admin();
        if let Some(mut info) = self.windows.get(&window_id) {
            if !info.is_open {
                self.env().revert(Error::WindowAlreadyClosed);
            }
            info.is_open = false;
            info.closed_at = self.env().get_block_time();
            self.windows.set(&window_id, info);
        } else {
            self.env().revert(Error::WindowNotFound);
        }
    }

    /// Returns true if the window exists and is open.
    pub fn is_open(&self, window_id: u64) -> bool {
        self.windows
            .get(&window_id)
            .map(|w| w.is_open)
            .unwrap_or(false)
    }

    /// Returns true if the window exists and is closed.
    pub fn is_closed(&self, window_id: u64) -> bool {
        self.windows
            .get(&window_id)
            .map(|w| !w.is_open)
            .unwrap_or(false)
    }

    /// Returns the most recently opened window_id (0 if no window has been opened).
    pub fn current_window_id(&self) -> u64 {
        self.active_wid.get_or_default()
    }

    /// Returns full window metadata, or None if window_id has never been opened.
    pub fn get_window(&self, window_id: u64) -> Option<WindowInfo> {
        self.windows.get(&window_id)
    }

    /// Returns the currently published crossing rule text.
    pub fn get_published_rule(&self) -> String {
        self.published_rule.get_or_default()
    }

    /// Returns the current rule version number.
    pub fn rule_version(&self) -> u32 {
        self.rule_ver.get_or_default()
    }

    /// Returns the rule text for a specific historical version.
    pub fn get_rule_at_version(&self, version: u32) -> Option<String> {
        self.rule_history.get(&version)
    }

    /// Publishes a new crossing rule and increments the rule version. Admin only.
    pub fn publish_rule(&mut self, rule: String) {
        self.assert_admin();
        let new_ver = self.rule_ver.get_or_default() + 1;
        self.rule_ver.set(new_ver);
        self.published_rule.set(rule.clone());
        self.rule_history.set(&new_ver, rule);
    }
}

impl WindowRegistry {
    fn assert_admin(&self) {
        let caller = self.env().caller();
        let is_admin = self.admin.get().map(|a| a == caller).unwrap_or(false);
        if !is_admin {
            self.env().revert(Error::NotAdmin);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WindowRegistry, WindowRegistryInitArgs};
    use odra::host::Deployer;
    use odra::prelude::*;

    const RULE: &str = "uniform-price crossing, max-volume, min-imbalance, midpoint tie-break";

    #[test]
    fn open_window_is_open() {
        let env = odra_test::env();
        let mut registry = WindowRegistry::deploy(
            &env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );

        let wid = registry.open_window();

        assert!(registry.is_open(wid));
        assert!(!registry.is_closed(wid));
        assert_eq!(registry.current_window_id(), wid);
    }

    #[test]
    fn close_window_is_closed() {
        let env = odra_test::env();
        let mut registry = WindowRegistry::deploy(
            &env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );
        let wid = registry.open_window();

        registry.close_window(wid);

        assert!(!registry.is_open(wid));
        assert!(registry.is_closed(wid));
    }

    #[test]
    fn close_already_closed_reverts() {
        let env = odra_test::env();
        let mut registry = WindowRegistry::deploy(
            &env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );
        let wid = registry.open_window();
        registry.close_window(wid);

        let result = registry.try_close_window(wid);
        assert!(result.is_err());
    }

    #[test]
    fn non_admin_cannot_open_window() {
        let env = odra_test::env();
        let mut registry = WindowRegistry::deploy(
            &env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );
        let mallory = env.get_account(1);

        env.set_caller(mallory);
        let result = registry.try_open_window();
        assert!(result.is_err());
    }

    #[test]
    fn publish_rule_increments_version() {
        let env = odra_test::env();
        let mut registry = WindowRegistry::deploy(
            &env,
            WindowRegistryInitArgs { initial_rule: RULE.to_string() },
        );

        assert_eq!(registry.rule_version(), 1);
        assert_eq!(registry.get_published_rule(), RULE.to_string());

        let new_rule = "new rule v2".to_string();
        registry.publish_rule(new_rule.clone());

        assert_eq!(registry.rule_version(), 2);
        assert_eq!(registry.get_published_rule(), new_rule);
        assert_eq!(registry.get_rule_at_version(1), Some(RULE.to_string()));
        assert_eq!(registry.get_rule_at_version(2), Some(new_rule));
    }
}
