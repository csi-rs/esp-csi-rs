/// CSI Collection Configuration Struct
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
pub struct CsiConfig {
    /// Enable to receive legacy long training field(lltf) data.
    pub lltf_en: bool,
    /// Enable to receive HT long training field(htltf) data.
    pub htltf_en: bool,
    /// Enable to receive space time block code HT long training
    /// field(stbc-htltf2) data.
    pub stbc_htltf2_en: bool,
    /// Enable to generate htlft data by averaging lltf and ht_ltf data when
    /// receiving HT packet. Otherwise, use ht_ltf data directly.
    pub ltf_merge_en: bool,
    /// Enable to turn on channel filter to smooth adjacent sub-carrier. Disable
    /// it to keep independence of adjacent sub-carrier.
    pub channel_filter_en: bool,
    /// Manually scale the CSI data by left shifting or automatically scale the
    /// CSI data. If set true, please set the shift bits. false: automatically.
    /// true: manually.
    pub manu_scale: bool,
    /// Manually left shift bits of the scale of the CSI data. The range of the
    /// left shift bits is 0~15.
    pub shift: u8,
    /// Enable to dump 802.11 ACK frame.
    pub dump_ack_en: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg(feature = "esp32c6")]
pub struct CsiConfig {
    /// Note: the default C6 config enables legacy/HT20/HT40 acquisition and ACK
    /// dump. For HT40-only collection, set `acquire_csi_legacy = 0`,
    /// `acquire_csi_ht20 = 0`, and `dump_ack_en = 0`.
    /// Enable to acquire CSI.
    pub enable: u32,
    /// Enable to acquire L-LTF when receiving a 11g PPDU.
    pub acquire_csi_legacy: u32,
    /// Enable to acquire HT-LTF when receiving an HT20 PPDU.
    pub acquire_csi_ht20: u32,
    /// Enable to acquire HT-LTF when receiving an HT40 PPDU.
    pub acquire_csi_ht40: u32,
    /// Value 0-3.
    pub val_scale_cfg: u32,
    /// Enable to dump 802.11 ACK frame, default disabled.
    pub dump_ack_en: u32,
    /// Reserved.
    pub reserved: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg(feature = "esp32c5")]
pub struct CsiConfig {
    /// Note: the default C5 config enables legacy/HT20/HT40 acquisition and ACK
    /// dump. For HT40-only collection, set `acquire_csi_legacy = 0`,
    /// `acquire_csi_ht20 = 0`, and `dump_ack_en = 0`.
    /// Enable to acquire CSI.
    pub enable: u32,
    /// Enable to acquire L-LTF.
    pub acquire_csi_legacy: u32,
    /// Force-acquire L-LTF.
    pub acquire_csi_force_lltf: bool,
    /// Enable to acquire HT-LTF when receiving an HT20 PPDU.
    pub acquire_csi_ht20: u32,
    /// Enable to acquire HT-LTF when receiving an HT40 PPDU.
    pub acquire_csi_ht40: u32,
    /// Enable to acquire VHT-LTF when receiving a VHT20 PPDU.
    pub acquire_csi_vht: bool,
    /// Value 0-3.
    pub val_scale_cfg: u32,
    /// Enable to dump 802.11 ACK frame, default disabled.
    pub dump_ack_en: u32,
    /// Reserved.
    pub reserved: u32,
}

impl Default for CsiConfig {
    /// Default implmentation for CSI Collection Configuration:
    /// - lltf is enabled
    /// - htltfis enabled
    /// - stbc htltf2 is enabled
    /// - ltf merge is enabled
    /// - channel filter is enabled
    /// - manu scale is disabled
    /// - no bit shift
    /// - 802.11 ack frame dump disabled
    #[cfg(not(any(feature = "esp32c5", feature = "esp32c6")))]
    fn default() -> Self {
        Self {
            lltf_en: true,
            htltf_en: true,
            stbc_htltf2_en: true,
            ltf_merge_en: true,
            channel_filter_en: false,
            manu_scale: false,
            shift: 0,
            dump_ack_en: false,
        }
    }
    // This CSI configuration is specific to the ESP32 C6 devices
    #[cfg(feature = "esp32c6")]
    fn default() -> Self {
        Self {
            enable: 1,
            acquire_csi_legacy: 1,
            acquire_csi_ht20: 1,
            acquire_csi_ht40: 1,
            val_scale_cfg: 2,
            // Enabled by default in IDF profile; disable for HT40-only capture.
            dump_ack_en: 1,
            reserved: 19,
        }
    }
    // ESP32-C5 (mac_version 3) — adds force_lltf and vht fields.
    #[cfg(feature = "esp32c5")]
    fn default() -> Self {
        Self {
            enable: 1,
            acquire_csi_legacy: 1,
            acquire_csi_force_lltf: true,
            acquire_csi_ht20: 1,
            acquire_csi_ht40: 1,
            acquire_csi_vht: true,
            val_scale_cfg: 2,
            // Enabled by default in IDF profile; disable for HT40-only capture.
            dump_ack_en: 1,
            reserved: 0,
        }
    }
}
