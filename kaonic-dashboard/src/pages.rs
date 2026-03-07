use std::str::FromStr;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect};
use axum::Form;
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde::Deserialize;

use kaonic_settings::settings::Settings;
use kaonic_settings::{
    BandwidthFilter, GatewayConfig, Hertz, KaonicCtrlConfig, Modulation, OfdmBandwidthOption,
    OfdmMcs, OfdmModulation, QpskChipFrequency, QpskModulation, QpskRateMode, RadioConfig,
};

pub type SharedSettings = Arc<Mutex<Settings>>;

// ── Query params ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct Flash {
    pub saved: Option<String>,
    pub error: Option<String>,
}

// ── Form structs ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SettingsForm {
    // VPN
    network: String,
    announce_freq_secs: String,
    peers: String,
    // Radio interface connection
    radio_listen: String,
    radio_server: String,
    radio_module: String,
    // RF config (optional)
    radio_freq_mhz: String,
    radio_channel_spacing_khz: String,
    radio_channel: String,
    radio_bandwidth_filter: String,
    // Modulation
    modulation_type: String, // "off" | "ofdm" | "qpsk"
    // OFDM
    ofdm_mcs: String,
    ofdm_opt: String,
    ofdm_pdt: String,
    ofdm_tx_power: String,
    // QPSK
    qpsk_fchip: String,
    qpsk_mode: String,
    qpsk_tx_power: String,
}

// ── CSS ───────────────────────────────────────────────────────────────────────

const CSS: &str = r#"
*{box-sizing:border-box;margin:0;padding:0}
body{font-family:system-ui,sans-serif;background:#0f1117;color:#e2e8f0;min-height:100vh}
a{color:#7c85f5;text-decoration:none}a:hover{text-decoration:underline}
header{background:#1a1d27;border-bottom:1px solid #2d3147;padding:.85rem 2rem;display:flex;align-items:center;gap:2rem}
header h1{font-size:1.1rem;font-weight:700;color:#7c85f5;letter-spacing:.02em}
header nav a{color:#94a3b8;font-size:.88rem;margin-right:1.2rem}
header nav a:hover{color:#e2e8f0}
main{max-width:740px;margin:2rem auto;padding:0 1.25rem}
.card{background:#1a1d27;border:1px solid #2d3147;border-radius:8px;padding:1.5rem;margin-bottom:1.5rem}
.card-title{font-size:.78rem;font-weight:700;color:#7c85f5;text-transform:uppercase;letter-spacing:.08em;margin-bottom:1.1rem}
.field{margin-bottom:1rem}
.field label{display:block;font-size:.83rem;color:#94a3b8;margin-bottom:.3rem;font-weight:500}
.field input,.field textarea,.field select{width:100%;background:#0f1117;border:1px solid #2d3147;border-radius:4px;padding:.45rem .7rem;color:#e2e8f0;font-size:.9rem;font-family:inherit}
.field input:focus,.field textarea:focus,.field select:focus{outline:none;border-color:#7c85f5;box-shadow:0 0 0 2px #7c85f520}
.field textarea{resize:vertical;min-height:90px;line-height:1.5;font-family:monospace}
.field small{display:block;color:#64748b;font-size:.76rem;margin-top:.25rem}
.row{display:grid;grid-template-columns:1fr 1fr;gap:1rem}
.row3{display:grid;grid-template-columns:1fr 1fr 1fr;gap:1rem}
.row4{display:grid;grid-template-columns:1fr 1fr 1fr 1fr;gap:1rem}
.actions{display:flex;justify-content:flex-end;gap:.75rem;margin-top:.5rem}
.btn{padding:.45rem 1.2rem;border-radius:4px;border:none;cursor:pointer;font-size:.9rem;font-weight:600}
.btn-primary{background:#7c85f5;color:#fff}.btn-primary:hover{background:#6470f0}
.flash{border-radius:6px;padding:.7rem 1rem;margin-bottom:1.5rem;font-size:.88rem}
.flash-ok{background:#14532d;border:1px solid #16a34a;color:#86efac}
.flash-err{background:#450a0a;border:1px solid #dc2626;color:#fca5a5}
.hint{font-size:.78rem;color:#64748b;margin-top:-.5rem;margin-bottom:.75rem}
.mod-section{display:none}.mod-section.active{display:block}
.slider-row{display:flex;align-items:center;gap:.75rem}
.slider-row input[type=range]{flex:1;accent-color:#7c85f5;cursor:pointer}
.slider-row input[type=number]{width:72px;flex-shrink:0;text-align:center}
select option{background:#1a1d27}
"#;

const JS: &str = r#"
function updateMod() {
  var t = document.getElementById('modulation_type').value;
  document.querySelectorAll('.mod-section').forEach(function(el) {
    el.classList.toggle('active', el.dataset.mod === t);
  });
}
function syncSlider(sliderId, numberId) {
  var s = document.getElementById(sliderId);
  var n = document.getElementById(numberId);
  s.addEventListener('input', function() { n.value = s.value; });
  n.addEventListener('input', function() { s.value = n.value; });
}
document.addEventListener('DOMContentLoaded', function() {
  updateMod();
  syncSlider('ofdm_tx_power_range', 'ofdm_tx_power');
  syncSlider('qpsk_tx_power_range', 'qpsk_tx_power');
});
"#;

// ── Layout wrapper ────────────────────────────────────────────────────────────

fn layout(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width,initial-scale=1";
                title { (title) " — Kaonic Gateway" }
                style { (PreEscaped(CSS)) }
            }
            body {
                header {
                    h1 { "⚡ Kaonic Gateway" }
                    nav {
                        a href="/" { "Dashboard" }
                        a href="/settings" { "Settings" }
                    }
                }
                main { (body) }
                script { (PreEscaped(JS)) }
            }
        }
    }
}

// ── Helpers — enum to form value / option rendering ──────────────────────────

fn mod_type_str(m: Option<&Modulation>) -> &'static str {
    match m {
        Some(Modulation::Ofdm(_)) => "ofdm",
        Some(Modulation::Qpsk(_)) => "qpsk",
        _ => "off",
    }
}

macro_rules! select_field {
    ($id:expr, $name:expr, $label:expr, $current:expr, $( ($val:expr, $text:expr) ),* $(,)?) => {
        html! {
            div .field {
                label for=($id) { ($label) }
                select id=($id) name=($name) {
                    $( option value=($val) selected[($current) == ($val)] { ($text) } )*
                }
            }
        }
    }
}

// ── GET /settings ─────────────────────────────────────────────────────────────

pub async fn get_settings(
    State(settings): State<SharedSettings>,
    Query(flash): Query<Flash>,
) -> impl IntoResponse {
    let config = {
        let s = settings.lock().unwrap();
        s.load_config().unwrap_or_else(|_| default_config())
    };

    let peers_text = config.peers.join("\n");
    let radio = config.kaonic_ctrl_config.as_ref();

    // RF config values
    let rf = radio.and_then(|r| r.radio_config);
    let freq_mhz = rf.map(|r| r.freq.as_mhz()).unwrap_or(869);
    let ch_spacing_khz = rf.map(|r| r.channel_spacing.as_khz()).unwrap_or(200);
    let channel = rf.map(|r| r.channel).unwrap_or(0);
    let bw_filter = rf.map(|r| r.bandwidth_filter).unwrap_or(BandwidthFilter::Narrow);

    // Modulation values
    let modulation = radio.and_then(|r| r.modulation);
    let mod_type = mod_type_str(modulation.as_ref());
    let ofdm = match &modulation { Some(Modulation::Ofdm(o)) => *o, _ => OfdmModulation::default() };
    let qpsk = match &modulation { Some(Modulation::Qpsk(q)) => *q, _ => QpskModulation::default() };

    let content = html! {
        h2 style="font-size:1.25rem;font-weight:700;margin-bottom:1.5rem" { "Configuration" }

        @if flash.saved.is_some() {
            div .flash.flash-ok { "✓ Settings saved successfully." }
        }
        @if let Some(err) = &flash.error {
            div .flash.flash-err { "✗ " (err) }
        }

        form method="post" action="/settings" {

            // ── VPN ─────────────────────────────────────────────────────────
            div .card {
                p .card-title { "VPN" }
                div .field {
                    label for="network" { "Network (CIDR)" }
                    input type="text" id="network" name="network"
                          value=(config.network) placeholder="10.20.0.0/16" required;
                    small { "IPv4 subnet shared by all VPN peers." }
                }
                div .row {
                    div .field {
                        label for="announce_freq_secs" { "Announce interval (s)" }
                        input type="number" id="announce_freq_secs" name="announce_freq_secs"
                              min="1" value=(config.announce_freq_secs);
                    }
                }
                div .field {
                    label for="peers" { "Peers" }
                    textarea id="peers" name="peers"
                             placeholder="fb08aff16ec6f5ccf0d3eb179028e9c3\n..." {
                        (peers_text)
                    }
                    small { "One Reticulum destination hash per line." }
                }
            }

            // ── Radio Interface ──────────────────────────────────────────────
            div .card {
                p .card-title { "Radio Interface (kaonic-ctrl)" }
                p .hint { "Leave both address fields empty to disable the radio interface." }
                div .row {
                    div .field {
                        label for="radio_listen" { "Listen address" }
                        input type="text" id="radio_listen" name="radio_listen"
                              value=(radio.map(|r| r.listen_addr.to_string()).unwrap_or_default())
                              placeholder="0.0.0.0:3001";
                    }
                    div .field {
                        label for="radio_server" { "Server address" }
                        input type="text" id="radio_server" name="radio_server"
                              value=(radio.map(|r| r.server_addr.to_string()).unwrap_or_default())
                              placeholder="192.168.10.1:3000";
                    }
                }
                div .field style="max-width:180px" {
                    label for="radio_module" { "Module index" }
                    input type="number" id="radio_module" name="radio_module"
                          min="0" value=(radio.map(|r| r.module).unwrap_or(0));
                }
            }

            // ── RF Config ────────────────────────────────────────────────────
            div .card {
                p .card-title { "RF Configuration" }
                p .hint { "Carrier frequency and channel plan. Applied when radio interface is enabled." }
                div .row {
                    div .field {
                        label for="radio_freq_mhz" { "Frequency (MHz)" }
                        input type="number" id="radio_freq_mhz" name="radio_freq_mhz"
                              min="0" value=(freq_mhz) placeholder="869";
                        small { "E.g. 869 for 869 MHz." }
                    }
                    div .field {
                        label for="radio_channel_spacing_khz" { "Channel spacing (kHz)" }
                        input type="number" id="radio_channel_spacing_khz"
                              name="radio_channel_spacing_khz" min="0"
                              value=(ch_spacing_khz) placeholder="200";
                    }
                }
                div .row {
                    div .field {
                        label for="radio_channel" { "Channel" }
                        input type="number" id="radio_channel" name="radio_channel"
                              min="0" value=(channel) placeholder="0";
                    }
                    (select_field!(
                        "radio_bandwidth_filter", "radio_bandwidth_filter", "Bandwidth filter",
                        if bw_filter == BandwidthFilter::Narrow { "narrow" } else { "wide" },
                        ("narrow", "Narrow"), ("wide", "Wide"),
                    ))
                }
            }

            // ── Modulation ───────────────────────────────────────────────────
            div .card {
                p .card-title { "Modulation" }
                div .field {
                    label for="modulation_type" { "Modulation type" }
                    select id="modulation_type" name="modulation_type"
                           onchange="updateMod()" {
                        option value="off"  selected[mod_type == "off"]  { "Off" }
                        option value="ofdm" selected[mod_type == "ofdm"] { "OFDM" }
                        option value="qpsk" selected[mod_type == "qpsk"] { "QPSK" }
                    }
                }

                // OFDM section
                div .mod-section .active[mod_type == "ofdm"] data-mod="ofdm" {
                    div .row {
                        (select_field!(
                            "ofdm_mcs", "ofdm_mcs", "MCS (Modulation & Coding Scheme)",
                            ofdm.mcs as u8,
                            (0u8, "BPSK 1/2 4x (slowest)"),
                            (1u8, "BPSK 1/2 2x"),
                            (2u8, "QPSK 1/2 2x"),
                            (3u8, "QPSK 1/2"),
                            (4u8, "QPSK 3/4"),
                            (5u8, "16-QAM 1/2"),
                            (6u8, "16-QAM 3/4 (fastest)"),
                        ))
                        (select_field!(
                            "ofdm_opt", "ofdm_opt", "Bandwidth option",
                            ofdm.opt as u8,
                            (0u8, "Option 1"), (1u8, "Option 2"),
                            (2u8, "Option 3"), (3u8, "Option 4"),
                        ))
                    }
                    div .row {
                        div .field {
                            label for="ofdm_pdt" { "Preamble detection threshold" }
                            input type="number" id="ofdm_pdt" name="ofdm_pdt"
                                  min="0" max="255" value=(ofdm.pdt);
                        }
                        div .field {
                            label for="ofdm_tx_power" { "TX power (dBm)" }
                            div .slider-row {
                                input type="range" id="ofdm_tx_power_range"
                                      min="0" max="30" value=(ofdm.tx_power);
                                input type="number" id="ofdm_tx_power" name="ofdm_tx_power"
                                      min="0" max="30" value=(ofdm.tx_power);
                            }
                        }
                    }
                }

                // QPSK section
                div .mod-section .active[mod_type == "qpsk"] data-mod="qpsk" {
                    div .row {
                        (select_field!(
                            "qpsk_fchip", "qpsk_fchip", "Chip frequency",
                            qpsk.fchip as u8,
                            (0u8, "100 kchip/s"), (1u8, "200 kchip/s"),
                            (2u8, "1000 kchip/s"), (3u8, "2000 kchip/s"),
                        ))
                        (select_field!(
                            "qpsk_mode", "qpsk_mode", "Rate mode",
                            qpsk.mode as u8,
                            (0u8, "Mode 0"), (1u8, "Mode 1"), (2u8, "Mode 2"),
                            (3u8, "Mode 3"), (4u8, "Mode 4"),
                        ))
                    }
                    div .field style="max-width:340px" {
                        label for="qpsk_tx_power" { "TX power (dBm)" }
                        div .slider-row {
                            input type="range" id="qpsk_tx_power_range"
                                  min="0" max="30" value=(qpsk.tx_power);
                            input type="number" id="qpsk_tx_power" name="qpsk_tx_power"
                                  min="0" max="30" value=(qpsk.tx_power);
                        }
                    }
                }
            }

            div .actions {
                button .btn.btn-primary type="submit" { "Save settings" }
            }
        }
    };

    layout("Settings", content)
}

// ── POST /settings ────────────────────────────────────────────────────────────

pub async fn post_settings(
    State(settings): State<SharedSettings>,
    Form(form): Form<SettingsForm>,
) -> impl IntoResponse {
    let network = match cidr::Ipv4Cidr::from_str(form.network.trim()) {
        Ok(n) => n,
        Err(_) => {
            return Redirect::to(&format!(
                "/settings?error={}",
                urlenc(&format!("Invalid network CIDR: {}", form.network.trim()))
            ));
        }
    };

    let announce_freq_secs = form.announce_freq_secs.trim().parse::<u32>().unwrap_or(1).max(1);
    let peers = form
        .peers
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();

    // Parse optional radio interface connection
    let listen_str = form.radio_listen.trim();
    let server_str = form.radio_server.trim();
    let kaonic_ctrl_config = if !listen_str.is_empty() && !server_str.is_empty() {
        let listen_addr = match listen_str.parse() {
            Ok(a) => a,
            Err(_) => return Redirect::to(&format!("/settings?error={}", urlenc(&format!("Invalid listen address: {listen_str}")))),
        };
        let server_addr = match server_str.parse() {
            Ok(a) => a,
            Err(_) => return Redirect::to(&format!("/settings?error={}", urlenc(&format!("Invalid server address: {server_str}")))),
        };
        let module = form.radio_module.trim().parse::<usize>().unwrap_or(0);

        // RF config
        let freq_mhz = form.radio_freq_mhz.trim().parse::<u64>().unwrap_or(0);
        let ch_spacing_khz = form.radio_channel_spacing_khz.trim().parse::<u64>().unwrap_or(0);
        let channel = form.radio_channel.trim().parse::<u16>().unwrap_or(0);
        let bw_filter = if form.radio_bandwidth_filter.trim() == "wide" {
            BandwidthFilter::Wide
        } else {
            BandwidthFilter::Narrow
        };
        let radio_config = if freq_mhz > 0 {
            Some(RadioConfig {
                freq: Hertz::from_mhz(freq_mhz),
                channel_spacing: Hertz::from_khz(ch_spacing_khz),
                channel,
                bandwidth_filter: bw_filter,
            })
        } else {
            None
        };

        // Modulation
        let modulation = match form.modulation_type.trim() {
            "ofdm" => {
                let mcs = match form.ofdm_mcs.trim().parse::<u8>().unwrap_or(0) {
                    0 => OfdmMcs::BpskC1_2_4x, 1 => OfdmMcs::BpskC1_2_2x,
                    2 => OfdmMcs::QpskC1_2_2x, 3 => OfdmMcs::QpskC1_2,
                    4 => OfdmMcs::QpskC3_4,    5 => OfdmMcs::QamC1_2,
                    _ => OfdmMcs::QamC3_4,
                };
                let opt = match form.ofdm_opt.trim().parse::<u8>().unwrap_or(0) {
                    1 => OfdmBandwidthOption::Option2, 2 => OfdmBandwidthOption::Option3,
                    3 => OfdmBandwidthOption::Option4, _ => OfdmBandwidthOption::Option1,
                };
                let pdt = form.ofdm_pdt.trim().parse::<u8>().unwrap_or(3);
                let tx_power = form.ofdm_tx_power.trim().parse::<u8>().unwrap_or(10);
                Some(Modulation::Ofdm(OfdmModulation { mcs, opt, pdt, tx_power }))
            }
            "qpsk" => {
                let fchip = match form.qpsk_fchip.trim().parse::<u8>().unwrap_or(0) {
                    1 => QpskChipFrequency::Fchip200, 2 => QpskChipFrequency::Fchip1000,
                    3 => QpskChipFrequency::Fchip2000, _ => QpskChipFrequency::Fchip100,
                };
                let mode = match form.qpsk_mode.trim().parse::<u8>().unwrap_or(0) {
                    1 => QpskRateMode::RateMode1, 2 => QpskRateMode::RateMode2,
                    3 => QpskRateMode::RateMode3, 4 => QpskRateMode::RateMode4,
                    _ => QpskRateMode::RateMode0,
                };
                let tx_power = form.qpsk_tx_power.trim().parse::<u8>().unwrap_or(10);
                Some(Modulation::Qpsk(QpskModulation { fchip, mode, tx_power }))
            }
            _ => Some(Modulation::Off),
        };

        Some(KaonicCtrlConfig { listen_addr, server_addr, module, radio_config, modulation })
    } else {
        None
    };

    let config = GatewayConfig { network, peers, announce_freq_secs, kaonic_ctrl_config };

    match settings.lock().unwrap().save_config(&config) {
        Ok(_) => Redirect::to("/settings?saved=1"),
        Err(err) => {
            log::error!("failed to save settings: {err}");
            Redirect::to(&format!("/settings?error={}", urlenc("Failed to save settings")))
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn default_config() -> GatewayConfig {
    GatewayConfig {
        network: "10.20.0.0/16".parse().unwrap(),
        peers: vec![],
        announce_freq_secs: 1,
        kaonic_ctrl_config: None,
    }
}

fn urlenc(s: &str) -> String {
    s.replace(' ', "+")
        .replace('"', "%22")
        .replace('&', "%26")
        .replace(':', "%3A")
}
