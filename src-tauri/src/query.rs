use actix_web::web::Buf;
use byteorder::{LittleEndian, ReadBytesExt};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind, Read};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{net::Ipv4Addr, time::Duration};
use tokio::net::{lookup_host, UdpSocket};
use tokio::time::timeout_at;
use tokio::time::Instant;

use crate::helpers;

static OMP_EXTRA_INFO_LAST_UPDATE_LIST: Lazy<Mutex<HashMap<String, u64>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
const OMP_EXTRA_INFO_UPDATE_COOLDOWN_SECS: u64 = 3;

pub struct Query {
    address: Ipv4Addr,
    port: i32,
    socket: UdpSocket,
}

#[derive(Serialize, Deserialize, Default)]
pub struct InfoPacket {
    pub password: bool,
    pub players: u16,
    pub max_players: u16,
    pub hostname: String,
    pub gamemode: String,
    pub language: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Player {
    pub name: String,
    pub score: i32,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ExtraInfoPacket {
    pub discord_link: String,
    pub light_banner_url: String,
    pub dark_banner_url: String,
    pub logo_url: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ServerQueryResponse {
    pub info: Option<String>,
    pub extra_info: Option<String>,
    pub players: Option<String>,
    pub rules: Option<String>,
    pub ping: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct ErrorResponse {
    pub info: String,
    pub error: bool,
}

impl Query {
    pub async fn new(addr: &str, port: i32) -> Result<Self, std::io::Error> {
        let regex = Regex::new(r"^(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)$").unwrap();

        let address = match regex.captures(addr) {
            Some(_) => {
                // it's valid ipv4, move on
                addr.to_string()
            }
            None => {
                let socket_addresses = lookup_host(format!("{}:{}", addr, port)).await;
                match socket_addresses {
                    Ok(s) => {
                        let mut ipv4 = "".to_string();
                        for socket_address in s {
                            if socket_address.is_ipv4() {
                                // hostname is resolved to ipv4:port, lets split it by ":" and get ipv4 only
                                let ip_port = socket_address.to_string();
                                let vec: Vec<&str> = ip_port.split(':').collect();
                                ipv4 = vec[0].to_string();
                            }
                        }
                        ipv4
                    }
                    Err(e) => {
                        println!("{}", e.to_string());
                        "".to_string()
                    }
                }
            }
        };

        let data = Self {
            address: address.parse::<Ipv4Addr>().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Invalid IP address: {}", e)))?,
            port,
            socket: UdpSocket::bind("0.0.0.0:0").await.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to bind UDP socket: {}", e)))?,
        };

        data.socket
            .connect(format!("{}:{}", addr, port))
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to connect to server: {}", e)))?;
        Ok(data)
    }

    pub async fn send(&self, query_type: char) -> Result<usize, std::io::Error> {
        let mut packet: Vec<u8> = Vec::new();
        packet.append(&mut "SAMP".to_owned().into_bytes());
        for i in 0..4 {
            packet.push(self.address.octets()[i]);
        }
        packet.push((self.port & 0xFF) as u8);
        packet.push((self.port >> 8 & 0xFF) as u8);
        packet.push(query_type as u8);

        if query_type == 'p' {
            packet.push(0);
            packet.push(0);
            packet.push(0);
            packet.push(0);
        }

        let amt = self.socket.send(&packet).await.unwrap();
        Ok(amt)
    }

    pub async fn recv(&self) -> Result<String, std::io::Error> {
        let mut buf = [0; 1500];
        let amt = match timeout_at(
            Instant::now() + Duration::from_secs(2),
            self.socket.recv(&mut buf),
        )
        .await?
        {
            Ok(n) => n,
            Err(e) => return Err(e),
        };

        if amt == 0 {
            return Err(Error::new(ErrorKind::Other, String::from("no_data")));
        }

        let query_type = buf[10] as char;
        let packet = Cursor::new(buf[11..amt].to_vec());
        if query_type == 'i' {
            self.build_info_packet(packet)
        } else if query_type == 'c' {
            self.build_players_packet(packet)
        } else if query_type == 'r' {
            self.build_rules_packet(packet)
        } else if query_type == 'o' {
            self.build_extra_info_packet(packet)
        } else if query_type == 'p' {
            Ok(String::from("pong"))
        } else {
            Err(Error::new(ErrorKind::Other, String::from("no_data")))
        }
    }

    fn build_info_packet(&self, mut packet: Cursor<Vec<u8>>) -> Result<String, std::io::Error> {
        let mut data = InfoPacket {
            password: packet.read_i8().unwrap() != 0,
            players: packet.read_u16::<LittleEndian>().unwrap(),
            max_players: packet.read_u16::<LittleEndian>().unwrap(),
            ..Default::default()
        };

        let hostname_len = packet.read_u32::<LittleEndian>().unwrap();
        let mut hostname_buf = vec![0u8; hostname_len as usize];
        packet.read_exact(&mut hostname_buf).unwrap();
        data.hostname = helpers::decode_buffer(hostname_buf).0;

        let gamemode_len = packet.read_u32::<LittleEndian>().unwrap();
        let mut gamemode_buf = vec![0u8; gamemode_len as usize];
        packet.read_exact(&mut gamemode_buf).unwrap();
        data.gamemode = helpers::decode_buffer(gamemode_buf).0;

        let language_len = packet.read_u32::<LittleEndian>().unwrap();
        let mut language_buf = vec![0u8; language_len as usize];
        packet.read_exact(&mut language_buf).unwrap();
        data.language = helpers::decode_buffer(language_buf).0;

        Ok(serde_json::to_string(&data).unwrap())
    }

    fn build_extra_info_packet(
        &self,
        mut packet: Cursor<Vec<u8>>,
    ) -> Result<String, std::io::Error> {
        let mut data = ExtraInfoPacket::default();

        let discord_link_len = packet.read_u32::<LittleEndian>().unwrap();
        let mut discord_link_buf = vec![0u8; discord_link_len as usize];
        packet.read_exact(&mut discord_link_buf).unwrap();
        data.discord_link = helpers::decode_buffer(discord_link_buf).0;

        let mut banner_url_len = packet.read_u32::<LittleEndian>().unwrap();
        let mut banner_url_buf = vec![0u8; banner_url_len as usize];
        packet.read_exact(&mut banner_url_buf).unwrap();
        data.light_banner_url = helpers::decode_buffer(banner_url_buf).0;

        banner_url_len = packet.read_u32::<LittleEndian>().unwrap();
        banner_url_buf = vec![0u8; banner_url_len as usize];
        packet.read_exact(&mut banner_url_buf).unwrap();
        data.dark_banner_url = helpers::decode_buffer(banner_url_buf).0;

        if packet.remaining() > 0 {
            let logo_url_len = packet.read_u32::<LittleEndian>().unwrap();
            let mut logo_url_buf = vec![0u8; logo_url_len as usize];
            packet.read_exact(&mut logo_url_buf).unwrap();
            data.logo_url = helpers::decode_buffer(logo_url_buf).0;
        }

        Ok(serde_json::to_string(&data).unwrap())
    }

    fn build_players_packet(&self, mut packet: Cursor<Vec<u8>>) -> Result<String, std::io::Error> {
        let player_count = packet.read_u16::<LittleEndian>().unwrap();
        let default_player = Player::default();
        let mut players = vec![default_player; player_count as usize];

        for i in 0..player_count {
            let player = &mut players[i as usize];

            let player_name_len = packet.read_u8().unwrap();
            let mut player_name_buf = vec![0u8; player_name_len as usize];
            packet.read_exact(&mut player_name_buf).unwrap();
            player.name = helpers::decode_buffer(player_name_buf).0;

            player.score = packet.read_i32::<LittleEndian>().unwrap();
        }

        Ok(serde_json::to_string(&players).unwrap())
    }

    fn build_rules_packet(&self, mut packet: Cursor<Vec<u8>>) -> Result<String, std::io::Error> {
        let rule_count = packet.read_u16::<LittleEndian>().unwrap();
        let mut rules: Vec<Vec<String>> = Vec::new();

        for _ in 0..rule_count {
            let mut rule: Vec<String> = Vec::new();

            let rule_name_len = packet.read_u8().unwrap();
            let mut rule_name_buf = vec![0u8; rule_name_len as usize];
            packet.read_exact(&mut rule_name_buf).unwrap();
            rule.push(helpers::decode_buffer(rule_name_buf).0);

            let rule_value_len = packet.read_u8().unwrap();
            let mut rule_value_buf = vec![0u8; rule_value_len as usize];
            packet.read_exact(&mut rule_value_buf).unwrap();
            rule.push(helpers::decode_buffer(rule_value_buf).0);

            rules.push(rule);
        }

        Ok(serde_json::to_string(&rules).unwrap())
    }
}

#[tauri::command]
pub async fn query_server(
    ip: &str,
    port: i32,
    info: bool,
    extra_info: bool,
    players: bool,
    rules: bool,
    ping: bool,
) -> Result<String, String> {
    match Query::new(ip, port).await {
        Ok(q) => {
            let mut result = ServerQueryResponse {
                info: None,
                extra_info: None,
                players: None,
                rules: None,
                ping: None,
            };

            if info {
                let _ = q.send('i').await;
                result.info = Some(match q.recv().await {
                    Ok(p) => format!("{}", p),
                    Err(e) => {
                        let mut error_details = ErrorResponse::default();
                        error_details.error = true;
                        error_details.info = e.to_string();
                        serde_json::to_string(&error_details).unwrap()
                    }
                });
            }

            if players {
                let _ = q.send('c').await;
                result.players = Some(match q.recv().await {
                    Ok(p) => format!("{}", p),
                    Err(e) => {
                        let mut error_details = ErrorResponse::default();
                        error_details.error = true;
                        error_details.info = e.to_string();
                        serde_json::to_string(&error_details).unwrap()
                    }
                });
            }

            if rules {
                let _ = q.send('r').await;
                result.rules = Some(match q.recv().await {
                    Ok(p) => format!("{}", p),
                    Err(e) => {
                        let mut error_details = ErrorResponse::default();
                        error_details.error = true;
                        error_details.info = e.to_string();
                        serde_json::to_string(&error_details).unwrap()
                    }
                });
            }

            if extra_info {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                let key = format!("{}:{}", ip, port);

                let should_request = {
                    let mut map = OMP_EXTRA_INFO_LAST_UPDATE_LIST.lock().unwrap();
                    match map.get(&key) {
                        Some(&last_time)
                            if now - last_time < OMP_EXTRA_INFO_UPDATE_COOLDOWN_SECS =>
                        {
                            false
                        }
                        _ => {
                            map.insert(key.clone(), now);
                            true
                        }
                    }
                };

                if should_request {
                    let _ = q.send('o').await;
                    result.extra_info = Some(match q.recv().await {
                        Ok(p) => format!("{}", p),
                        Err(e) => {
                            let mut error_details = ErrorResponse::default();
                            error_details.error = true;
                            error_details.info = e.to_string();
                            serde_json::to_string(&error_details).unwrap()
                        }
                    });
                }
            }

            if ping {
                let _ = q.send('p').await;
                let before = Instant::now();
                match q.recv().await {
                    Ok(_p) => {
                        result.ping = Some(before.elapsed().as_millis() as u32);
                    }
                    Err(_) => {
                        result.ping = Some(9999);
                    }
                }
            }

            Ok(serde_json::to_string(&result).unwrap())
        }
        Err(e) => Err(e.to_string()),
    }
}
