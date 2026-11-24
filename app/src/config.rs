use std::path::PathBuf;

pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            port: std::env::var("PANGGUNG_PORT")
                .unwrap_or_else(|_| "3000".to_owned())
                .parse()?,
            data_dir: PathBuf::from(
                std::env::var_os("PANGGUNG_DATA_DIR").unwrap_or_else(|| "data".into()),
            ),
        })
    }
}
