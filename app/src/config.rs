use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseProvider {
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectStorageProvider {
    Local,
}

#[derive(Debug)]
pub struct Config {
    pub port: u16,
    pub data_dir: PathBuf,
    pub database_provider: DatabaseProvider,
    pub storage_provider: ObjectStorageProvider,
}

impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let database = optional_env("PANGGUNG_DATABASE_PROVIDER")?;
        let storage = optional_env("PANGGUNG_STORAGE_PROVIDER")?;
        let (database_provider, storage_provider) =
            providers(database.as_deref(), storage.as_deref())?;
        Ok(Self {
            port: std::env::var("PANGGUNG_PORT")
                .unwrap_or_else(|_| "3000".to_owned())
                .parse()?,
            data_dir: PathBuf::from(
                std::env::var_os("PANGGUNG_DATA_DIR").unwrap_or_else(|| "data".into()),
            ),
            database_provider,
            storage_provider,
        })
    }
}

fn optional_env(name: &str) -> Result<Option<String>, std::io::Error> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must contain valid Unicode"),
        )),
    }
}

fn providers(
    database: Option<&str>,
    storage: Option<&str>,
) -> Result<(DatabaseProvider, ObjectStorageProvider), std::io::Error> {
    let database = match database.unwrap_or("sqlite") {
        "sqlite" => DatabaseProvider::Sqlite,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported PANGGUNG_DATABASE_PROVIDER; expected sqlite",
            ));
        }
    };
    let storage = match storage.unwrap_or("local") {
        "local" => ObjectStorageProvider::Local,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported PANGGUNG_STORAGE_PROVIDER; expected local",
            ));
        }
    };
    Ok((database, storage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_explicit_providers_match() {
        let expected = (DatabaseProvider::Sqlite, ObjectStorageProvider::Local);
        assert_eq!(providers(None, None).unwrap(), expected);
        assert_eq!(providers(Some("sqlite"), Some("local")).unwrap(), expected);
    }

    #[test]
    fn rejects_unsupported_database_providers() {
        for value in ["postgres", "", "SQLite"] {
            let error = providers(Some(value), None).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("PANGGUNG_DATABASE_PROVIDER"));
        }
    }

    #[test]
    fn rejects_unsupported_storage_providers() {
        for value in ["s3", "", "Local"] {
            let error = providers(None, Some(value)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("PANGGUNG_STORAGE_PROVIDER"));
        }
    }
}
