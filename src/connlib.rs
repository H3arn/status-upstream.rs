/*
 ** Copyright (C) 2021-2022 KunoiSayami
 **
 ** This program is free software: you can redistribute it and/or modify
 ** it under the terms of the GNU Affero General Public License as published by
 ** the Free Software Foundation, either version 3 of the License, or
 ** any later version.
 **
 ** This program is distributed in the hope that it will be useful,
 ** but WITHOUT ANY WARRANTY; without even the implied warranty of
 ** MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
 ** GNU Affero General Public License for more details.
 **
 ** You should have received a copy of the GNU Affero General Public License
 ** along with this program. If not, see <https://www.gnu.org/licenses/>.
 */

#[async_trait::async_trait]
pub trait ServiceChecker {
    async fn ping(&self, timeout: u64) -> anyhow::Result<bool>;
}

#[async_trait::async_trait]
impl<F: ?Sized + Sync + Send> ServiceChecker for Box<F>
where
    F: ServiceChecker + Sync + Send,
{
    async fn ping(&self, timeout: u64) -> anyhow::Result<bool> {
        (**self).ping(timeout).await
    }
}

#[derive(Copy, Clone, Debug)]
pub enum ServiceType {
    HTTP,
    SSH,
    TeamSpeak,
}

pub mod teamspeak {
    use crate::connlib::ServiceChecker;
    use tokio::net::UdpSocket;
    use tokio::time::Duration;

    const HEAD_DATA: [u8; 34] =
        hex_literal::hex!("545333494e49543100650000880ef967a500613f9e6966788d480000000000000000");

    pub struct TeamSpeak {
        remote_address: String,
    }

    impl TeamSpeak {
        pub fn new(remote_address: &str) -> Self {
            Self {
                remote_address: remote_address.to_string(),
            }
        }
    }
    #[async_trait::async_trait]
    impl ServiceChecker for TeamSpeak {
        // TODO: Support ipv6
        async fn ping(&self, timeout: u64) -> anyhow::Result<bool> {
            let socket = UdpSocket::bind("0.0.0.0:0").await?;

            socket.send_to(&HEAD_DATA, &self.remote_address).await?;

            //socket.set_read_timeout(Duration::from_secs(1));

            let mut buf = [0; 64];
            if let Ok((amt, _src)) =
                tokio::time::timeout(Duration::from_secs(timeout), socket.recv_from(&mut buf))
                    .await?
            {
                Ok(amt != 0)
            } else {
                Ok(false)
            }
        }
    }
}

pub mod ssh {

    use crate::connlib::ServiceChecker;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tokio::time::Duration;

    const HEAD_DATA: [u8; 21] = hex_literal::hex!("5353482d322e302d4f70656e5353485f382e370d0a");

    pub struct SSH {
        remote_address: String,
    }

    impl SSH {
        pub fn new(remote_address: &str) -> Self {
            Self {
                remote_address: remote_address.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ServiceChecker for SSH {
        async fn ping(&self, timeout: u64) -> anyhow::Result<bool> {
            if let Ok(mut socket) = tokio::time::timeout(
                Duration::from_secs(timeout),
                TcpStream::connect(&self.remote_address),
            )
            .await?
            {
                if let Ok(_) =
                    tokio::time::timeout(Duration::from_secs(timeout), socket.write_all(&HEAD_DATA))
                        .await?
                {
                    let mut buff = [0; 64];
                    if let Ok(_) =
                        tokio::time::timeout(Duration::from_secs(timeout), socket.read(&mut buff))
                            .await?
                    {
                        return Ok(String::from_utf8_lossy(&buff).contains("SSH"));
                    }
                }
            }
            Ok(false)
        }
    }
}

pub mod http {
    use crate::connlib::ServiceChecker;
    use reqwest::tls::Version;
    use reqwest::ClientBuilder;
    use std::time::Duration;

    pub struct HTTP {
        remote_address: String,
    }

    impl HTTP {
        pub fn new(remote_address: &str) -> Self {
            Self {
                remote_address: remote_address.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ServiceChecker for HTTP {
        async fn ping(&self, timeout: u64) -> anyhow::Result<bool> {
            let client = ClientBuilder::new()
                .timeout(Duration::from_secs(timeout))
                .min_tls_version(Version::TLS_1_2)
                .build()?;
            let req = client.get(&self.remote_address).send().await?;
            let status = req.status().as_u16();
            Ok((300 > status) && (status >= 200))
        }
    }
}

#[derive(Debug, Clone)]
pub enum ServerLastStatus {
    Optional,
    Outage,
    Unknown,
}

impl From<bool> for ServerLastStatus {
    fn from(b: bool) -> Self {
        if b {
            Self::Optional
        } else {
            Self::Outage
        }
    }
}

impl PartialEq<bool> for ServerLastStatus {
    fn eq(&self, other: &bool) -> bool {
        match self {
            ServerLastStatus::Optional => *other,
            _ => !other,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ServiceWrapper {
    last_status: ServerLastStatus,
    remote_address: String,
    report_uuid: String,
    service_type: ServiceType,
}

impl ServiceWrapper {
    pub fn report_uuid(&self) -> &str {
        &self.report_uuid
    }
    pub fn service_type(&self) -> &ServiceType {
        &self.service_type
    }

    pub async fn ping(&self, timeout: u64) -> anyhow::Result<bool> {
        match self.service_type() {
            ServiceType::HTTP => HTTP::new(&self.remote_address).ping(timeout).await,
            ServiceType::SSH => SSH::new(&self.remote_address).ping(timeout).await,
            ServiceType::TeamSpeak => TeamSpeak::new(&self.remote_address).ping(timeout).await,
        }
    }
    pub fn last_status(&self) -> &ServerLastStatus {
        &self.last_status
    }
    pub fn remote_address(&self) -> &str {
        &self.remote_address
    }

    pub fn update_last_status(&mut self, last_status: bool) -> bool {
        if self.last_status != last_status {
            self.last_status = ServerLastStatus::from(last_status);
            true
        } else {
            false
        }
    }
}

impl TryFrom<&Service> for ServiceWrapper {
    type Error = anyhow::Error;

    fn try_from(s: &Service) -> Result<Self, Self::Error> {
        let service_type = s.service_type().to_lowercase();
        let service_type = match service_type.as_str() {
            "teamspeak" | "ts" => ServiceType::TeamSpeak,
            "ssh" => ServiceType::SSH,
            "http" => ServiceType::HTTP,
            &_ => {
                return Err(anyhow!(
                    "Unexpect service type: {}, report uuid => {}",
                    s.service_type(),
                    s.report_uuid()
                ));
            }
        };

        Ok(Self {
            last_status: ServerLastStatus::Optional,
            report_uuid: s.report_uuid().to_string(),
            service_type,
            remote_address: s.remote_address().to_string(),
        })
    }
}


use anyhow::anyhow;
pub use http::HTTP;
pub use ssh::SSH;
pub use teamspeak::TeamSpeak;
use crate::configure::Service;
