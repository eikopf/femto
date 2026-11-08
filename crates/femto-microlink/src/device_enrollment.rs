//! In-memory approval continuation bound to coordinator and local identity.
use crate::{
    device_config::Config,
    enrollment::ApprovalUrl,
    protocol::{
        CAPABILITY_VERSION,
        control::{HostInfo, RegisterRequest},
        identity::DeviceIdentity,
        key::{Machine, NetworkLock, Node, PublicKey},
    },
};

#[derive(Clone, PartialEq, Eq)]
pub struct Context {
    host: String,
    port: u16,
    machine: PublicKey<Machine>,
    node: PublicKey<Node>,
    old_node: PublicKey<Node>,
}
#[derive(Clone)]
pub struct Followup {
    context: Context,
    url: ApprovalUrl,
}
impl Context {
    pub fn new(config: &Config, identity: &DeviceIdentity) -> Self {
        Self {
            host: config.host.to_ascii_lowercase(),
            port: config.port,
            machine: identity.machine_key().public(),
            node: identity.node_key().public(),
            old_node: PublicKey::zero(),
        }
    }
    pub fn for_rotation(
        config: &Config,
        pending: &crate::rotation::PendingRotation,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        pending
            .validate_endpoint(&config.host, config.port)
            .map_err(|_| "Rotation belongs to a different coordinator")?;
        let mut context = Self::new(config, pending.candidate());
        context.old_node = pending.old_node_key();
        Ok(context)
    }
    pub fn followup(&self, url: ApprovalUrl) -> Followup {
        Followup {
            context: self.clone(),
            url,
        }
    }
    pub fn validate(&self, followup: &Followup) -> Result<(), &'static str> {
        if *self != followup.context {
            return Err(
                "Approval belongs to a different coordinator or identity; disconnect to clear it",
            );
        }
        Ok(())
    }
    /// Follow-up registration omits an auth key. The URL is sent only in the
    /// authenticated coordinator request; it is never fetched by this module.
    pub fn registration(
        &self,
        network_lock: PublicKey<NetworkLock>,
        host: HostInfo,
        auth: Option<&str>,
        followup: Option<&Followup>,
    ) -> Result<RegisterRequest, &'static str> {
        let request = RegisterRequest::new(CAPABILITY_VERSION, self.node, network_lock, host)
            .rotating_from(self.old_node);
        if let Some(followup) = followup {
            self.validate(followup)?;
            Ok(request.with_followup(followup.url.as_str()))
        } else if let Some(auth) = auth {
            Ok(request.with_auth_key(auth))
        } else {
            Ok(request)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(host: &str, port: u16) -> Config {
        Config::parse(
            format!(r#"{{"version":1,"host":"{host}","port":{port},"hostname":"femto"}}"#)
                .as_bytes(),
        )
        .unwrap()
    }
    #[test]
    fn rotation_approval_is_bound_to_candidate_and_old_key_relationship() {
        let identity = DeviceIdentity::generate().unwrap();
        let pending =
            crate::rotation::PendingRotation::prepare(&identity, "control.example", 443).unwrap();
        let config = config("control.example", 443);
        let context = Context::for_rotation(&config, &pending).unwrap();
        let followup = context.followup(ApprovalUrl::parse("https://login.example/token").unwrap());
        assert!(
            Context::new(&config, &identity)
                .validate(&followup)
                .is_err()
        );
        assert!(
            Context::new(&config, pending.candidate())
                .validate(&followup)
                .is_err()
        );
        let request = context
            .registration(
                pending.candidate().network_lock_key().public(),
                HostInfo::esp32("femto", ""),
                Some("omitted-secret"),
                Some(&followup),
            )
            .unwrap();
        assert_eq!(request.node_key, pending.candidate().node_key().public());
        assert_eq!(request.old_node_key, identity.node_key().public());
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire["Followup"], "https://login.example/token");
        assert!(wire.get("Auth").is_none());
    }

    #[test]
    fn followup_is_bound_to_coordinator_port_and_identity() {
        let identity = DeviceIdentity::generate().unwrap();
        let context = Context::new(&config("control.example", 443), &identity);
        let followup =
            context.followup(ApprovalUrl::parse("https://login.example/approve/token").unwrap());
        assert!(
            Context::new(&config("CONTROL.EXAMPLE", 443), &identity)
                .validate(&followup)
                .is_ok()
        );
        assert!(
            Context::new(&config("other.example", 443), &identity)
                .validate(&followup)
                .is_err()
        );
        assert!(
            Context::new(&config("control.example", 8443), &identity)
                .validate(&followup)
                .is_err()
        );
        assert!(
            Context::new(
                &config("control.example", 443),
                &DeviceIdentity::generate().unwrap()
            )
            .validate(&followup)
            .is_err()
        );
        let request = context
            .registration(
                identity.network_lock_key().public(),
                HostInfo::esp32("femto", ""),
                Some("must-not-be-sent"),
                Some(&followup),
            )
            .unwrap();
        let wire = serde_json::to_value(request).unwrap();
        assert_eq!(wire["Followup"], "https://login.example/approve/token");
        assert!(!wire.to_string().contains("must-not-be-sent"));
    }
}
