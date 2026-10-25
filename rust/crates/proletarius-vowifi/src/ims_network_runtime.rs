//! Single network-task composition for the live IMS registration owner.
//!
//! The registration coordinator needs dynamic TCP listeners/dials and the
//! security transform that protects Port-S/Port-C traffic. These controls
//! must share one `TunnelNetwork` task so command acknowledgements and packet
//! processing have one ordering boundary. The runtime deliberately exposes
//! control handles only; socket state and IMS security keys stay in the
//! network task.

use crate::{
    netstack_runtime::{NetworkRuntime, PacketDirection, PacketTransformOutcome},
    netstack_tcp::{TcpControl, TcpSocketRegistry},
    netstack_udp::{UdpControl, UdpSocketRegistry},
    security_runtime::{SecurityControl, SecurityNetworkRuntime},
};
use smoltcp::iface::{Interface, SocketSet};
use std::{
    convert::Infallible,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;
use zeroize::Zeroizing;

/// Read-only receipt from the exact network owner. A business/peer error is
/// independent of whether its local sockets and security keys were retired.
#[derive(Clone)]
pub struct Retirement(Arc<AtomicBool>);
impl Retirement {
    pub fn confirmed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct ImsNetworkRuntime {
    retirement: Retirement,
    tcp: TcpSocketRegistry,
    udp: UdpSocketRegistry,
    security: SecurityNetworkRuntime<NetworkService>,
}

type NetworkService = fn(&mut Interface, &mut SocketSet<'static>) -> Result<(), Infallible>;

impl ImsNetworkRuntime {
    pub fn retirement(&self) -> Retirement {
        self.retirement.clone()
    }

    /// Creates the control handles consumed by `RegistrationServices` and the
    /// single runtime that must be passed to `NativeTunnel::run_network_with_runtime`.
    /// Both control planes wake that same network owner.
    pub fn new(wake: watch::Sender<u64>) -> (TcpControl, SecurityControl, Self) {
        let (tcp, security, _udp, runtime) = Self::new_with_udp(wake);
        (tcp, security, runtime)
    }

    /// RTP and signaling share one authenticated tunnel and one network owner.
    /// UDP packets never leave via a separate host network socket.
    pub fn new_with_udp(
        wake: watch::Sender<u64>,
    ) -> (TcpControl, SecurityControl, UdpControl, Self) {
        let (tcp_control, tcp) = TcpSocketRegistry::new(wake.clone());
        let (udp_control, udp) = UdpSocketRegistry::new(wake.clone());
        let (security_control, security) =
            SecurityNetworkRuntime::new(no_application_work as NetworkService, wake);
        (
            tcp_control,
            security_control,
            udp_control,
            Self {
                tcp,
                udp,
                security,
                retirement: Retirement(Arc::new(AtomicBool::new(false))),
            },
        )
    }
}

fn no_application_work(
    _interface: &mut Interface,
    _sockets: &mut SocketSet<'static>,
) -> Result<(), Infallible> {
    Ok(())
}

impl NetworkRuntime for ImsNetworkRuntime {
    type ApplicationError = Infallible;

    fn service(
        &mut self,
        interface: &mut Interface,
        sockets: &mut SocketSet<'static>,
    ) -> Result<(), Self::ApplicationError> {
        self.tcp.service(interface, sockets);
        self.udp.service(interface, sockets);
        NetworkRuntime::service(&mut self.security, interface, sockets)
    }

    fn retire(&mut self, sockets: &mut SocketSet<'static>) {
        self.tcp.retire(sockets);
        self.udp.retire(sockets);
        self.security.retire();
        self.retirement.0.store(true, Ordering::Release);
    }

    fn transform_packet(
        &mut self,
        direction: PacketDirection,
        packet: Zeroizing<Vec<u8>>,
    ) -> Result<PacketTransformOutcome, io::Error> {
        NetworkRuntime::transform_packet(&mut self.security, direction, packet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TunnelConfiguration, netstack_runtime::TunnelNetwork};
    use proletarius_ims::{
        aka_exchange::SecurityKeys,
        security_offer::parse_security_server,
        security_policy::{LocalEndpoints, SecurityPolicy},
    };
    use smoltcp::{iface::SocketSet, wire::Ipv4Address};
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::mpsc,
    };

    fn network(ip: [u8; 4]) -> TunnelNetwork {
        TunnelNetwork::new(&TunnelConfiguration {
            ipv4_addresses: vec![Ipv4Address::new(ip[0], ip[1], ip[2], ip[3])],
            ..Default::default()
        })
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn composed_runtime_routes_ims_tcp_and_stops_both_control_planes() {
        let client_network = network([10, 0, 0, 2]);
        let server_network = network([10, 20, 30, 9]);
        let (client_wake_tx, client_wake) = watch::channel(0_u64);
        let (server_wake_tx, server_wake) = watch::channel(0_u64);
        let (client_tcp, client_security, client_runtime) = ImsNetworkRuntime::new(client_wake_tx);
        let (server_tcp, _server_security, server_runtime) = ImsNetworkRuntime::new(server_wake_tx);
        let (client_to_server, server_inbound) = mpsc::channel(256);
        let (server_to_client, client_inbound) = mpsc::channel(256);
        let (stop_tx, stop) = watch::channel(false);

        let client_runner = client_network.run_managed(
            SocketSet::new(vec![]),
            client_inbound,
            client_to_server,
            stop.clone(),
            client_wake,
            client_runtime,
        );
        let server_runner = server_network.run_managed(
            SocketSet::new(vec![]),
            server_inbound,
            server_to_client,
            stop.clone(),
            server_wake,
            server_runtime,
        );
        let exchange = async {
            let offer = parse_security_server(
                "ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6064;port-s=6063",
            )
            .unwrap()
            .remove(0);
            let policy = SecurityPolicy::aes_sha1(
                "10.0.0.2".parse().unwrap(),
                "10.20.30.9".parse().unwrap(),
                LocalEndpoints {
                    spi_c: 333,
                    spi_s: 444,
                    port_c: 5064,
                    port_s: 5063,
                },
                &offer,
                SecurityKeys {
                    ck: Zeroizing::new([1; 16]),
                    ik: Zeroizing::new([2; 16]),
                },
            )
            .unwrap();
            client_security
                .install(policy)
                .await
                .unwrap()
                .clear()
                .await
                .unwrap();

            let mut listener = server_tcp
                .listen(5060, Some("10.0.0.2:32000".parse().unwrap()))
                .await
                .unwrap();
            let pending = client_tcp
                .dial("10.20.30.9:5060".parse().unwrap(), 32000)
                .await
                .unwrap();
            let (client, server) = tokio::join!(pending.connect(), listener.accept_cancel_safe());
            let mut client = client.unwrap();
            let mut server = server.unwrap();

            let register = b"REGISTER sip:ims.example.test SIP/2.0\r\nContent-Length: 0\r\n\r\n";
            client.write_all(register).await.unwrap();
            let mut received = vec![0; register.len()];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(received, register);

            let response = b"SIP/2.0 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
            server.write_all(response).await.unwrap();
            let mut received_response = vec![0; response.len()];
            client.read_exact(&mut received_response).await.unwrap();
            assert_eq!(received_response, response);
            client.close().await.unwrap();
            server.close().await.unwrap();
            stop_tx.send(true).unwrap();
        };

        let (client_result, server_result, ()) =
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(client_runner, server_runner, exchange)
            })
            .await
            .expect("composed IMS network runtime did not stop");
        assert!(client_result.is_ok());
        assert!(server_result.is_ok());
    }
    async fn retirement_case(mode: u8) {
        let net = network([10, 0, 0, 2]);
        let (wake, changes) = watch::channel(0);
        let (tcp, security, udp, runtime) = ImsNetworkRuntime::new_with_udp(wake);
        let retired = runtime.retirement();
        let (stop_tx, stop) = watch::channel(false);
        let (in_tx, input) = mpsc::channel(8);
        let (output, _out_rx) = mpsc::channel(8);
        let owner = tokio::spawn(net.run_managed(
            SocketSet::new(vec![]),
            input,
            output,
            stop,
            changes,
            runtime,
        ));
        let listener = tcp.listen(5063, None).await.unwrap();
        let mut packet = udp.bind("10.0.0.2".parse().unwrap()).await.unwrap();
        let offer = parse_security_server("ipsec-3gpp;alg=hmac-sha-1-96;ealg=aes-cbc;prot=esp;mod=trans;spi-c=111;spi-s=222;port-c=6064;port-s=6063").unwrap().remove(0);
        let policy = SecurityPolicy::aes_sha1(
            "10.0.0.2".parse().unwrap(),
            "10.20.30.9".parse().unwrap(),
            LocalEndpoints {
                spi_c: 333,
                spi_s: 444,
                port_c: 5064,
                port_s: 5063,
            },
            &offer,
            SecurityKeys {
                ck: Zeroizing::new([1; 16]),
                ik: Zeroizing::new([2; 16]),
            },
        )
        .unwrap();
        let security_lease = security.install(policy).await.unwrap();
        assert!(!retired.confirmed());
        match mode {
            0 => {
                stop_tx.send_replace(true);
                assert!(owner.await.unwrap().is_ok());
            }
            1 => {
                drop(in_tx);
                assert!(owner.await.unwrap().is_err());
            }
            _ => {
                owner.abort();
                assert!(owner.await.unwrap_err().is_cancelled());
            }
        }
        assert!(
            retired.confirmed(),
            "exact runtime retirement must follow socket/key removal"
        );
        assert!(tcp.is_closed());
        listener.close().await.unwrap();
        packet.close().await.unwrap();
        assert!(udp.bind("10.0.0.2".parse().unwrap()).await.is_err());
        // A late security lease cannot resurrect keys or claim successful IO.
        assert!(security_lease.clear().await.is_err());
    }
    #[tokio::test]
    async fn retirement_receipt_normal_stop_removes_tcp_udp_and_security() {
        retirement_case(0).await;
    }
    #[tokio::test]
    async fn retirement_receipt_preserves_network_failure_after_local_cleanup() {
        retirement_case(1).await;
    }
    #[tokio::test]
    async fn retirement_receipt_task_abort_still_waits_for_actual_socket_removal() {
        retirement_case(2).await;
    }
}
