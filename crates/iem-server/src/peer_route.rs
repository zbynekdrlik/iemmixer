//! `GET /api/peer` (S7 HIL v2, design §7 "the tunnel peer"): how this server
//! classified the request it answers, by `login_guard`'s rule.

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::util::ServiceExt;

    use crate::login_guard::{HostAddrs, LoginGuard};

    /// A client's address as the tunnel connector names it (TEST-NET-3).
    const CLIENT: &str = "203.0.113.7";
    /// The host's own LAN address (the public placeholder) and a foreign
    /// LAN peer.
    const HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10));
    const FOREIGN: IpAddr = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 50));
    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// The real API routes over a state whose login guard trusts
    /// CF-Connecting-IP from `host` (and loopback), reached from `peer`.
    fn app(dir: &std::path::Path, peer: IpAddr, host: &[IpAddr]) -> Router {
        let (mut state, _) = crate::routes::api_tests::app(dir);
        state.login_guard = Arc::new(LoginGuard::for_host(HostAddrs::new(host.iter().copied())));
        crate::routes::api_routes(state.clone())
            .with_state(state)
            .layer(MockConnectInfo(SocketAddr::new(peer, 40000)))
    }

    /// `GET /api/peer` with an optional CF-Connecting-IP: the status and the
    /// body as it came.
    async fn peer(app: &Router, cf: Option<&str>) -> (StatusCode, String) {
        let mut req = Request::builder().uri("/api/peer");
        if let Some(ip) = cf {
            req = req.header("cf-connecting-ip", ip);
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn answer(app: &Router, cf: Option<&str>) -> Value {
        let (status, body) = peer(app, cf).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    #[tokio::test]
    async fn a_loopback_peer_with_cf_connecting_ip_is_the_tunnel() {
        let dir = tempfile::tempdir().unwrap();
        let v4 = app(dir.path(), LOOPBACK, &[]);
        assert_eq!(
            answer(&v4, Some(CLIENT)).await,
            json!({"origin": "tunnel", "peer": "loopback"})
        );
        // An IPv6 client through the connector, and the connector on ::1.
        assert_eq!(
            answer(&v4, Some("2001:db8::7")).await,
            json!({"origin": "tunnel", "peer": "loopback"})
        );
        let v6 = app(dir.path(), IpAddr::V6(Ipv6Addr::LOCALHOST), &[]);
        assert_eq!(
            answer(&v6, Some(CLIENT)).await,
            json!({"origin": "tunnel", "peer": "loopback"})
        );
    }

    #[tokio::test]
    async fn the_hosts_own_address_with_the_header_is_the_tunnel() {
        let dir = tempfile::tempdir().unwrap();
        let host = app(dir.path(), HOST, &[HOST]);
        assert_eq!(
            answer(&host, Some(CLIENT)).await,
            json!({"origin": "tunnel", "peer": "host"})
        );
        assert_eq!(
            answer(&host, None).await,
            json!({"origin": "lan", "peer": "host"})
        );
        // As an IPv4-mapped IPv6 peer (a dual-stack listener) too.
        let mapped = app(
            dir.path(),
            IpAddr::V6(Ipv4Addr::new(10, 0, 0, 10).to_ipv6_mapped()),
            &[HOST],
        );
        assert_eq!(
            answer(&mapped, Some(CLIENT)).await,
            json!({"origin": "tunnel", "peer": "host"})
        );
    }

    #[tokio::test]
    async fn a_loopback_peer_without_the_header_is_lan() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(dir.path(), LOOPBACK, &[HOST]);
        assert_eq!(
            answer(&app, None).await,
            json!({"origin": "lan", "peer": "loopback"})
        );
        // A header that is no address falls back to the peer, as for logins.
        assert_eq!(
            answer(&app, Some("not-an-address")).await,
            json!({"origin": "lan", "peer": "loopback"})
        );
    }

    #[tokio::test]
    async fn a_foreign_peer_with_a_forged_header_is_lan_and_other() {
        let dir = tempfile::tempdir().unwrap();
        for host in [&[][..], &[HOST][..]] {
            let foreign = app(dir.path(), FOREIGN, host);
            assert_eq!(
                answer(&foreign, Some(CLIENT)).await,
                json!({"origin": "lan", "peer": "other"}),
                "{host:?}"
            );
            assert_eq!(
                answer(&foreign, Some("127.0.0.1")).await,
                json!({"origin": "lan", "peer": "other"}),
                "{host:?}"
            );
            assert_eq!(
                answer(&foreign, None).await,
                json!({"origin": "lan", "peer": "other"}),
                "{host:?}"
            );
        }
        // The host's address is "host" only when this server's guard names
        // it as its own.
        let unknown = app(dir.path(), HOST, &[]);
        assert_eq!(
            answer(&unknown, Some(CLIENT)).await,
            json!({"origin": "lan", "peer": "other"})
        );
    }

    /// Fixed codes only (P6): exactly `origin` and `peer`, each a string, and
    /// neither the peer's nor the forwarded client's address anywhere.
    #[tokio::test]
    async fn the_answer_has_exactly_origin_and_peer() {
        let dir = tempfile::tempdir().unwrap();
        for (peer_ip, host) in [
            (LOOPBACK, &[][..]),
            (HOST, &[HOST][..]),
            (FOREIGN, &[HOST][..]),
        ] {
            let app = app(dir.path(), peer_ip, host);
            let (status, body) = peer(&app, Some(CLIENT)).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let v: Value = serde_json::from_str(&body).unwrap();
            let fields = v.as_object().unwrap();
            let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["origin", "peer"], "{body}");
            assert!(fields.values().all(Value::is_string), "{body}");
            for address in [CLIENT, &peer_ip.to_string(), "10.0.0", "127.0.0"] {
                assert!(!body.contains(address), "{address} in {body}");
            }
        }
    }
}
