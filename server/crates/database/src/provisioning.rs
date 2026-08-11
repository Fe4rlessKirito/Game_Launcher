use super::Database;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use launcher_provisioning::{
    ProvisionRequest, ProvisioningError, ProvisioningEvent, ProvisioningJob,
    ProvisioningMailRecord, ProvisioningStatus, ProvisioningStore, ProvisioningTransition,
    SecretRef,
};
use sqlx::{AssertSqlSafe, Row, postgres::PgRow};
use std::str::FromStr;
use uuid::Uuid;

const PROVISIONING_JOB_COLUMNS: &str = "id, provider_type, pool_id, requested_capacity_bytes,
    status, attempt_count, created_at, updated_at, started_at, completed_at,
    last_error_code, last_error_summary, inbound_email_token_hash,
    inbound_email_address, inbound_email_expires_at, candidate_reference,
    credential_reference, operator_action, retry_after, expires_at, idempotency_key";

fn database_error() -> ProvisioningError {
    // Do not copy database diagnostics into a provisioning job or HTTP response:
    // providers can echo input values in server errors.
    ProvisioningError::Provider("provisioning database operation failed".to_owned())
}

fn invalid_record() -> ProvisioningError {
    ProvisioningError::Provider("provisioning database record is invalid".to_owned())
}

fn validate_key(value: &str, field: &str) -> Result<(), ProvisioningError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(ProvisioningError::Security(format!("invalid {field}")));
    }
    Ok(())
}

fn select_job_sql(suffix: &str) -> String {
    format!("SELECT {PROVISIONING_JOB_COLUMNS} FROM provisioning_jobs {suffix}")
}

fn job_from_row(row: &PgRow) -> Result<ProvisioningJob, ProvisioningError> {
    let status_value: String = row.try_get("status").map_err(|_| invalid_record())?;
    let status = ProvisioningStatus::from_str(&status_value).map_err(|_| invalid_record())?;
    let requested_capacity_bytes = u64::try_from(
        row.try_get::<i64, _>("requested_capacity_bytes")
            .map_err(|_| invalid_record())?,
    )
    .map_err(|_| invalid_record())?;
    let credential_reference = row
        .try_get::<Option<String>, _>("credential_reference")
        .map_err(|_| invalid_record())?
        .map(|value| SecretRef::parse(value).map_err(|_| invalid_record()))
        .transpose()?;
    Ok(ProvisioningJob {
        id: row.try_get("id").map_err(|_| invalid_record())?,
        provider_type: row.try_get("provider_type").map_err(|_| invalid_record())?,
        pool_id: row.try_get("pool_id").map_err(|_| invalid_record())?,
        requested_capacity_bytes,
        status,
        attempt_count: u32::try_from(
            row.try_get::<i32, _>("attempt_count")
                .map_err(|_| invalid_record())?,
        )
        .map_err(|_| invalid_record())?,
        created_at: row.try_get("created_at").map_err(|_| invalid_record())?,
        updated_at: row.try_get("updated_at").map_err(|_| invalid_record())?,
        started_at: row.try_get("started_at").map_err(|_| invalid_record())?,
        completed_at: row.try_get("completed_at").map_err(|_| invalid_record())?,
        last_error_code: row
            .try_get("last_error_code")
            .map_err(|_| invalid_record())?,
        last_error_summary: row
            .try_get("last_error_summary")
            .map_err(|_| invalid_record())?,
        inbound_email_token_hash: row
            .try_get("inbound_email_token_hash")
            .map_err(|_| invalid_record())?,
        inbound_email_address: row
            .try_get("inbound_email_address")
            .map_err(|_| invalid_record())?,
        inbound_email_expires_at: row
            .try_get("inbound_email_expires_at")
            .map_err(|_| invalid_record())?,
        candidate_reference: row
            .try_get("candidate_reference")
            .map_err(|_| invalid_record())?,
        credential_reference,
        operator_action: row
            .try_get("operator_action")
            .map_err(|_| invalid_record())?,
        retry_after: row.try_get("retry_after").map_err(|_| invalid_record())?,
        expires_at: row.try_get("expires_at").map_err(|_| invalid_record())?,
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(|_| invalid_record())?,
    })
}

async fn find_job_by_id<'a>(
    executor: &mut sqlx::Transaction<'a, sqlx::Postgres>,
    id: Uuid,
    for_update: bool,
) -> Result<Option<ProvisioningJob>, ProvisioningError> {
    let suffix = if for_update {
        "WHERE id=$1 FOR UPDATE"
    } else {
        "WHERE id=$1"
    };
    sqlx::query(AssertSqlSafe(select_job_sql(suffix)))
        .bind(id)
        .fetch_optional(&mut **executor)
        .await
        .map_err(|_| database_error())?
        .as_ref()
        .map(job_from_row)
        .transpose()
}

#[async_trait]
impl ProvisioningStore for Database {
    async fn create_or_get_job(
        &self,
        request: ProvisionRequest,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        validate_key(&request.provider_type, "provider type")?;
        validate_key(&request.pool_id, "pool id")?;
        validate_key(&request.idempotency_key, "idempotency key")?;
        if request.expires_at <= Utc::now() {
            return Err(ProvisioningError::Configuration(
                "provisioning request is already expired".to_owned(),
            ));
        }
        let requested_capacity_bytes =
            i64::try_from(request.requested_capacity_bytes).map_err(|_| {
                ProvisioningError::Configuration("requested capacity is too large".to_owned())
            })?;
        let mut transaction = self.pool().begin().await.map_err(|_| database_error())?;
        let lock_key = format!("provisioning:{}:{}", request.provider_type, request.pool_id);
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(&mut *transaction)
            .await
            .map_err(|_| database_error())?;
        let existing = sqlx::query(AssertSqlSafe(select_job_sql(
            "WHERE idempotency_key=$1 OR (provider_type=$2 AND pool_id=$3
             AND status NOT IN ('ENROLLED','FAILED_PERMANENT','CANCELLED'))
             ORDER BY (idempotency_key=$1) DESC, created_at LIMIT 1",
        )))
        .bind(&request.idempotency_key)
        .bind(&request.provider_type)
        .bind(&request.pool_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        if let Some(row) = existing {
            let job = job_from_row(&row)?;
            transaction.commit().await.map_err(|_| database_error())?;
            return Ok(job);
        }
        let job = ProvisioningJob::new(request.clone());
        let inserted = sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO provisioning_jobs(
                id, provider_type, pool_id, requested_capacity_bytes, status,
                attempt_count, created_at, updated_at, expires_at, idempotency_key
             ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT DO NOTHING
             RETURNING {PROVISIONING_JOB_COLUMNS}"
        )))
        .bind(job.id)
        .bind(&job.provider_type)
        .bind(&job.pool_id)
        .bind(requested_capacity_bytes)
        .bind(job.status.as_str())
        .bind(i32::try_from(job.attempt_count).map_err(|_| invalid_record())?)
        .bind(job.created_at)
        .bind(job.updated_at)
        .bind(job.expires_at)
        .bind(&job.idempotency_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        let row = if let Some(row) = inserted {
            row
        } else {
            sqlx::query(AssertSqlSafe(select_job_sql(
                "WHERE idempotency_key=$1 OR (provider_type=$2 AND pool_id=$3
                 AND status NOT IN ('ENROLLED','FAILED_PERMANENT','CANCELLED'))
                 ORDER BY (idempotency_key=$1) DESC, created_at LIMIT 1",
            )))
            .bind(&job.idempotency_key)
            .bind(&job.provider_type)
            .bind(&job.pool_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|_| database_error())?
        };
        let job = job_from_row(&row)?;
        transaction.commit().await.map_err(|_| database_error())?;
        Ok(job)
    }

    async fn claim_start(&self, id: Uuid) -> Result<Option<ProvisioningJob>, ProvisioningError> {
        let mut transaction = self.pool().begin().await.map_err(|_| database_error())?;
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE provisioning_jobs
             SET status='STARTING', attempt_count=attempt_count+1,
                 started_at=COALESCE(started_at, now()), updated_at=now()
             WHERE id=$1 AND status='CREATED'
             RETURNING {PROVISIONING_JOB_COLUMNS}"
        )))
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        let Some(row) = row else {
            transaction.commit().await.map_err(|_| database_error())?;
            return Ok(None);
        };
        let job = job_from_row(&row)?;
        sqlx::query(
            "INSERT INTO provisioning_job_events(
                 job_id, idempotency_key, event_type, from_status, to_status, safe_summary
             ) VALUES($1,'job-start','START','CREATED','STARTING','job started')
             ON CONFLICT(job_id, idempotency_key) DO NOTHING",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        transaction.commit().await.map_err(|_| database_error())?;
        Ok(Some(job))
    }

    async fn get_job(&self, id: Uuid) -> Result<Option<ProvisioningJob>, ProvisioningError> {
        sqlx::query(AssertSqlSafe(select_job_sql("WHERE id=$1")))
            .bind(id)
            .fetch_optional(self.pool())
            .await
            .map_err(|_| database_error())?
            .as_ref()
            .map(job_from_row)
            .transpose()
    }

    async fn apply_event(
        &self,
        id: Uuid,
        idempotency_key: &str,
        event: ProvisioningEvent,
    ) -> Result<ProvisioningJob, ProvisioningError> {
        validate_key(idempotency_key, "event idempotency key")?;
        let mut transaction = self.pool().begin().await.map_err(|_| database_error())?;
        let already_applied: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                 SELECT 1 FROM provisioning_job_events
                 WHERE job_id=$1 AND idempotency_key=$2
             )",
        )
        .bind(id)
        .bind(idempotency_key)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        if already_applied {
            let job = find_job_by_id(&mut transaction, id, false)
                .await?
                .ok_or(ProvisioningError::NotFound)?;
            transaction.commit().await.map_err(|_| database_error())?;
            return Ok(job);
        }
        let mut job = find_job_by_id(&mut transaction, id, true)
            .await?
            .ok_or(ProvisioningError::NotFound)?;
        let transition: ProvisioningTransition = job.apply_event(event)?;
        let credential_reference = job
            .credential_reference
            .as_ref()
            .map(|reference| reference.as_str().to_owned());
        sqlx::query(
            "UPDATE provisioning_jobs SET
                status=$1, attempt_count=$2, updated_at=$3, started_at=$4,
                completed_at=$5, last_error_code=$6, last_error_summary=$7,
                inbound_email_token_hash=$8, inbound_email_address=$9,
                inbound_email_expires_at=$10, candidate_reference=$11,
                credential_reference=$12, operator_action=$13, retry_after=$14
             WHERE id=$15",
        )
        .bind(job.status.as_str())
        .bind(i32::try_from(job.attempt_count).map_err(|_| invalid_record())?)
        .bind(job.updated_at)
        .bind(job.started_at)
        .bind(job.completed_at)
        .bind(&job.last_error_code)
        .bind(&job.last_error_summary)
        .bind(&job.inbound_email_token_hash)
        .bind(&job.inbound_email_address)
        .bind(job.inbound_email_expires_at)
        .bind(&job.candidate_reference)
        .bind(credential_reference)
        .bind(&job.operator_action)
        .bind(job.retry_after)
        .bind(job.id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        sqlx::query(
            "INSERT INTO provisioning_job_events(
                 job_id, idempotency_key, event_type, from_status, to_status, safe_summary
             ) VALUES($1,$2,$3,$4,$5,$6)
             ON CONFLICT(job_id, idempotency_key) DO NOTHING",
        )
        .bind(job.id)
        .bind(idempotency_key)
        .bind(&transition.event_type)
        .bind(transition.from.as_str())
        .bind(transition.to.as_str())
        .bind(&transition.safe_summary)
        .execute(&mut *transaction)
        .await
        .map_err(|_| database_error())?;
        transaction.commit().await.map_err(|_| database_error())?;
        Ok(job)
    }

    async fn find_active_job_by_email(
        &self,
        address: &str,
    ) -> Result<Option<ProvisioningJob>, ProvisioningError> {
        validate_key(address, "email address")?;
        sqlx::query(AssertSqlSafe(select_job_sql(
            "WHERE lower(inbound_email_address)=lower($1)
             AND inbound_email_expires_at > now()
             AND status NOT IN ('ENROLLED','FAILED_PERMANENT','CANCELLED')
             ORDER BY created_at DESC LIMIT 1",
        )))
        .bind(address.trim())
        .fetch_optional(self.pool())
        .await
        .map_err(|_| database_error())?
        .as_ref()
        .map(job_from_row)
        .transpose()
    }

    async fn claim_mail_nonce(
        &self,
        nonce: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<bool, ProvisioningError> {
        validate_key(nonce, "mail nonce")?;
        sqlx::query("DELETE FROM provisioning_mail_nonces WHERE expires_at <= now()")
            .execute(self.pool())
            .await
            .map_err(|_| database_error())?;
        let result = sqlx::query(
            "INSERT INTO provisioning_mail_nonces(nonce, signed_at, expires_at)
             VALUES($1, now(), $2) ON CONFLICT(nonce) DO NOTHING",
        )
        .bind(nonce)
        .bind(expires_at)
        .execute(self.pool())
        .await
        .map_err(|_| database_error())?;
        Ok(result.rows_affected() == 1)
    }

    async fn record_mail(&self, record: ProvisioningMailRecord) -> Result<bool, ProvisioningError> {
        validate_key(&record.message_id, "message id")?;
        validate_key(&record.body_sha256, "mail body digest")?;
        validate_key(&record.envelope_to, "mail recipient")?;
        let result = sqlx::query(
            "INSERT INTO provisioning_mail_messages(
                 message_id, body_sha256, envelope_from, envelope_to,
                 parsed_from, subject, job_id
             ) VALUES($1,$2,$3,$4,$5,$6,$7)
             ON CONFLICT(message_id) DO NOTHING",
        )
        .bind(record.message_id)
        .bind(record.body_sha256)
        .bind(record.envelope_from)
        .bind(record.envelope_to)
        .bind(record.from_header)
        .bind(record.subject)
        .bind(record.job_id)
        .execute(self.pool())
        .await
        .map_err(|_| database_error())?;
        Ok(result.rows_affected() == 1)
    }

    async fn list_jobs(
        &self,
        status: Option<ProvisioningStatus>,
        limit: u32,
    ) -> Result<Vec<ProvisioningJob>, ProvisioningError> {
        let limit = i64::from(limit.clamp(1, 500));
        let rows = if let Some(status) = status {
            sqlx::query(AssertSqlSafe(select_job_sql(
                "WHERE status=$1 ORDER BY created_at DESC, id DESC LIMIT $2",
            )))
            .bind(status.as_str())
            .bind(limit)
            .fetch_all(self.pool())
            .await
            .map_err(|_| database_error())?
        } else {
            sqlx::query(AssertSqlSafe(select_job_sql(
                "ORDER BY created_at DESC, id DESC LIMIT $1",
            )))
            .bind(limit)
            .fetch_all(self.pool())
            .await
            .map_err(|_| database_error())?
        };
        rows.iter().map(job_from_row).collect()
    }
}
