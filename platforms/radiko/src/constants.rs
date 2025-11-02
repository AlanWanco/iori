/// The full key used for authentication
/// This is the aSmartPhone7a key extracted from the app
pub const FULL_KEY: &[u8] = include_bytes!("radiko_aSmartPhone7a.bin");

/// Coordinates for each Japanese prefecture (JP1-JP47)
/// Format: [latitude, longitude]
pub const COORDINATES: &[(f64, f64)] = &[
    (43.064615, 141.346807), // JP1  - Hokkaido
    (40.824308, 140.739998), // JP2  - Aomori
    (39.703619, 141.152684), // JP3  - Iwate
    (38.268837, 140.8721),   // JP4  - Miyagi
    (39.718614, 140.102364), // JP5  - Akita
    (38.240436, 140.363633), // JP6  - Yamagata
    (37.750299, 140.467551), // JP7  - Fukushima
    (36.341811, 140.446793), // JP8  - Ibaraki
    (36.565725, 139.883565), // JP9  - Tochigi
    (36.390668, 139.060406), // JP10 - Gunma
    (35.856999, 139.648849), // JP11 - Saitama
    (35.605057, 140.123306), // JP12 - Chiba
    (35.689488, 139.691706), // JP13 - Tokyo
    (35.447507, 139.642345), // JP14 - Kanagawa
    (37.902552, 139.023095), // JP15 - Niigata
    (36.695291, 137.211338), // JP16 - Toyama
    (36.594682, 136.625573), // JP17 - Ishikawa
    (36.065178, 136.221527), // JP18 - Fukui
    (35.664158, 138.568449), // JP19 - Yamanashi
    (36.651299, 138.180956), // JP20 - Nagano
    (35.391227, 136.722291), // JP21 - Gifu
    (34.97712, 138.383084),  // JP22 - Shizuoka
    (35.180188, 136.906565), // JP23 - Aichi
    (34.730283, 136.508588), // JP24 - Mie
    (35.004531, 135.86859),  // JP25 - Shiga
    (35.021247, 135.755597), // JP26 - Kyoto
    (34.686297, 135.519661), // JP27 - Osaka
    (34.691269, 135.183071), // JP28 - Hyogo
    (34.685334, 135.832742), // JP29 - Nara
    (34.225987, 135.167509), // JP30 - Wakayama
    (35.503891, 134.237736), // JP31 - Tottori
    (35.472295, 133.0505),   // JP32 - Shimane
    (34.661751, 133.934406), // JP33 - Okayama
    (34.39656, 132.459622),  // JP34 - Hiroshima
    (34.185956, 131.470649), // JP35 - Yamaguchi
    (34.065718, 134.55936),  // JP36 - Tokushima
    (34.340149, 134.043444), // JP37 - Kagawa
    (33.841624, 132.765681), // JP38 - Ehime
    (33.559706, 133.531079), // JP39 - Kochi
    (33.606576, 130.418297), // JP40 - Fukuoka
    (33.249442, 130.299794), // JP41 - Saga
    (32.744839, 129.873756), // JP42 - Nagasaki
    (32.789827, 130.741667), // JP43 - Kumamoto
    (33.238172, 131.612619), // JP44 - Oita
    (31.911096, 131.423893), // JP45 - Miyazaki
    (31.560146, 130.557978), // JP46 - Kagoshima
    (26.2124, 127.680932),   // JP47 - Okinawa
];

/// Android device models for spoofing
pub const MODELS: &[&str] = &[
    // Samsung Galaxy S7+
    "SC-02H",
    "SCV33",
    "SM-G935F",
    "SM-G935X",
    "SM-G935W8",
    "SM-G935K",
    // Samsung Galaxy Note
    "SC-01J",
    "SCV34",
    "SM-N930F",
    "SM-N930X",
    "SM-N930K",
    // KYOCERA
    "WX06K",
    "404KC",
    "503KC",
    "602KC",
    "KYV32",
    // Sony Xperia Z series
    "C6902",
    "C6903",
    "C6906",
    "SO-01F",
    "SOL23",
    // Sharp
    "605SH",
    "SH-03J",
    "SHV39",
    "701SH",
];

/// Android version information
pub struct AndroidVersion {
    pub version: &'static str,
    pub sdk: &'static str,
    pub builds: &'static [&'static str],
}

pub const ANDROID_VERSIONS: &[AndroidVersion] = &[
    AndroidVersion {
        version: "7.0.0",
        sdk: "24",
        builds: &["NBD92Q", "NBD92N", "NBD92G", "NBD92F", "NBD92E"],
    },
    AndroidVersion {
        version: "8.0.0",
        sdk: "26",
        builds: &["5650811", "5796467", "5948681"],
    },
    AndroidVersion {
        version: "9.0.0",
        sdk: "28",
        builds: &["5948683", "5794013", "6127072"],
    },
    AndroidVersion {
        version: "10.0.0",
        sdk: "29",
        builds: &["5933585", "6969601", "7023426", "7070703"],
    },
    AndroidVersion {
        version: "11.0.0",
        sdk: "30",
        builds: &["RP1A.201005.006", "RQ1A.201205.011", "RQ1A.210105.002"],
    },
    AndroidVersion {
        version: "12.0.0",
        sdk: "31",
        builds: &[
            "SD1A.210817.015.A4",
            "SD1A.210817.019.B1",
            "SQ1D.220105.007",
        ],
    },
];

/// App versions for spoofing
pub const APP_VERSIONS: &[&str] = &["7.5.0", "7.4.17", "7.4.16", "7.4.15", "7.4.14", "7.4.13"];
