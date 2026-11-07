use super::*;
use ring::hmac;
use std::sync::{Mutex as StdMutex, TryLockError};

static JOB_OPERATION_MUTEX: StdMutex<()> = StdMutex::new(());

#[derive(Debug, thiserror::Error)]
#[error("job operation admission is busy")]
pub struct JobOperationsBusy;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobOperation {
    pub id: String,
    pub store_id: String,
    pub thread_id: String,
    pub workspace: PathBuf,
    pub cwd: PathBuf,
    pub purpose: String,
    pub status: String,
    pub job_id: Option<String>,
    pub job_status: Option<String>,
    pub exit_code: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobOperationRecord {
    pub schema_version: u32,
    pub operation: JobOperation,
    pub request_fingerprint: String,
    pub owner_generation: String,
}

pub struct JobOperationTransaction<'a> {
    store: &'a RuntimeThreadStore,
}

pub fn validate_key(key: &str) -> Result<()> {
    let id = Uuid::parse_str(key).context("invalid job operation key")?;
    anyhow::ensure!(
        id.get_version_num() == 4 && id.to_string() == key,
        "invalid job operation key"
    );
    Ok(())
}

pub fn request_fingerprint(key: &str, request: &[u8]) -> Result<String> {
    validate_key(key)?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, key.as_bytes());
    Ok(hex_digest(hmac::sign(&key, request).as_ref()))
}

impl RuntimeThreadStore {
    pub fn with_job_operations<T>(
        &self,
        action: impl FnOnce(&JobOperationTransaction<'_>) -> Result<T>,
    ) -> Result<T> {
        let _local = match JOB_OPERATION_MUTEX.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Err(JobOperationsBusy.into()),
            Err(TryLockError::Poisoned(_)) => bail!("job operation admission unavailable"),
        };
        let root = self
            .job_operations_dir
            .parent()
            .context("missing runtime store root")?;
        checked_existing_runtime_store_dir(&self.job_operations_dir)?;
        let path = root.join("job-operations.lock");
        let file = open_runtime_store_file(&path, "job operation lock", |options| {
            options.create(true).truncate(false).read(true).write(true);
        })?;
        let mut lock = fd_lock::RwLock::new(file);
        let guard = match lock.try_write() {
            Ok(guard) => guard,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Err(JobOperationsBusy.into());
            }
            Err(error) => return Err(error).context("job operation lock unavailable"),
        };
        anyhow::ensure!(
            owner_lock_is_at_path(&guard, &path)?,
            "job operation lock changed"
        );
        action(&JobOperationTransaction { store: self })
    }
}

impl JobOperationTransaction<'_> {
    pub fn operation_id(&self, thread_id: &str, key: &str) -> Result<String> {
        validated_record_id(thread_id, "job operation thread")?;
        validate_key(key)?;
        Ok(codewhale_runtime::hashing::sha256_hex(format!(
            "runtime-job-operation\u{1f}{}\u{1f}{thread_id}\u{1f}{key}",
            self.store.owner_id
        )))
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        validate_sha256_fingerprint(id, "job operation id")?;
        RuntimeThreadStore::record_path(
            &self.store.job_operations_dir,
            id,
            "json",
            "job operation id",
        )
    }

    fn validate(&self, record: &JobOperationRecord) -> Result<()> {
        let operation = &record.operation;
        anyhow::ensure!(
            record.schema_version == 1,
            "unsupported job operation schema"
        );
        validate_sha256_fingerprint(&operation.id, "job operation id")?;
        validate_sha256_fingerprint(&record.request_fingerprint, "job request fingerprint")?;
        validated_record_id(&operation.thread_id, "job operation thread")?;
        anyhow::ensure!(
            operation.store_id == self.store.owner_id,
            "job operation store mismatch"
        );
        anyhow::ensure!(
            operation.workspace.is_absolute() && operation.cwd.is_absolute(),
            "invalid job operation workspace"
        );
        anyhow::ensure!(
            matches!(
                operation.purpose.as_str(),
                "setup" | "run" | "archive" | "command"
            ),
            "invalid job purpose"
        );
        anyhow::ensure!(
            matches!(
                operation.status.as_str(),
                "running" | "completed" | "rejected" | "unknown"
            ),
            "invalid job status"
        );
        if let Some(id) = &operation.job_id {
            validated_record_id(id, "job id")?;
            anyhow::ensure!(
                id.starts_with("shell_") && id.len() <= 128,
                "invalid job id"
            );
        }
        anyhow::ensure!(
            operation.created_at_ms > 0 && operation.updated_at_ms >= operation.created_at_ms,
            "invalid job operation timestamps"
        );
        anyhow::ensure!(
            operation.status == "completed"
                || (operation.job_status.is_none() && operation.exit_code.is_none()),
            "nonterminal job operation has a terminal result"
        );
        anyhow::ensure!(
            operation.status != "rejected" || operation.job_id.is_none(),
            "rejected job operation has a job id"
        );
        anyhow::ensure!(
            operation.status != "running" || operation.job_id.is_some(),
            "running job operation has no job id"
        );
        anyhow::ensure!(
            operation.status != "completed"
                || (operation.job_id.is_some()
                    && operation
                        .job_status
                        .as_deref()
                        .is_some_and(|status| matches!(
                            status,
                            "Completed" | "Failed" | "Killed" | "TimedOut"
                        ))),
            "completed job operation has no terminal result"
        );
        anyhow::ensure!(
            Uuid::parse_str(&record.owner_generation).is_ok_and(|id| id.get_version_num() == 4),
            "invalid job owner generation"
        );
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<JobOperationRecord>> {
        let path = self.path(id)?;
        let raw = match read_store_file(&path) {
            Ok(raw) => raw,
            Err(error)
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let record: JobOperationRecord = serde_json::from_str(&raw)?;
        self.validate(&record)?;
        anyhow::ensure!(record.operation.id == id, "job operation path mismatch");
        Ok(Some(record))
    }

    pub fn list(&self) -> Result<Vec<JobOperationRecord>> {
        let directory = checked_existing_runtime_store_dir(&self.store.job_operations_dir)?;
        let mut records = Vec::new();
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .context("invalid job operation filename")?;
            records.push(self.get(id)?.context("job operation disappeared")?);
        }
        records.sort_by(|a, b| {
            a.operation
                .created_at_ms
                .cmp(&b.operation.created_at_ms)
                .then_with(|| a.operation.id.cmp(&b.operation.id))
        });
        Ok(records)
    }

    pub fn save(&self, record: &JobOperationRecord) -> Result<()> {
        self.validate(record)?;
        write_json_atomic(&self.path(&record.operation.id)?, record)
    }
}
