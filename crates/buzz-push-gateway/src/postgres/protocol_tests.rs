use super::*;

fn assertion(
    previous: u32,
    next: u32,
    expected: Option<i64>,
    now: i64,
) -> Option<AssertionCounter> {
    Some(AssertionCounter {
        request_expires_at: None,
        previous,
        next,
        expected_generation: expected,
        now,
        quota_window_seconds: 60,
    })
}
fn delegation(hash: [u8; 32], relay: &str, generation: i64, now: i64) -> Delegation {
    Delegation {
        grant_hash: Some(hash),
        id: Uuid::from_bytes(hash[..16].try_into().unwrap()),
        installation_id: Uuid::from_u128(1),
        relay_pubkey: relay.to_owned(),
        endpoint_epoch: 1,
        generation,
        not_before: now,
        expires_at: now + 1_000,
        revoked: false,
    }
}
async fn deliver(
    store: &PostgresAuthorityStore,
    d: &Delegation,
    event: u8,
    request: Uuid,
    cap: i64,
    now: i64,
) -> Result<DeliveryPermit, AuthorityError> {
    store
        .authorize_delivery(
            d.id,
            d.grant_hash,
            &d.relay_pubkey,
            d.endpoint_epoch,
            d.generation,
            &hex::encode([event; 32]),
            request,
            now + 100,
            60,
            cap,
            now,
        )
        .await
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn opaque_hash_and_atomic_management_fence_legacy_and_delayed_authority() {
    let (pool, schema) = full_schema(4).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    // The expansion preserves a live legacy grant until its actual renewal.
    admit(&store, &"22".repeat(32), Uuid::new_v4())
        .await
        .unwrap();
    let d = delegation([5; 32], RELAY_HEX, 2, now);
    store
        .upsert_delegation(d.clone(), assertion(0, 1, Some(1), now))
        .await
        .unwrap();
    assert!(admit(&store, &"33".repeat(32), Uuid::new_v4())
        .await
        .is_err());
    let mut tampered = d.clone();
    tampered.grant_hash.as_mut().unwrap()[31] ^= 1; // Same indexed prefix, wrong full hash.
    assert!(deliver(&store, &tampered, 4, Uuid::new_v4(), 40, now)
        .await
        .is_err());
    let mut wrong_relay = d.clone();
    wrong_relay.relay_pubkey = "66".repeat(32);
    assert!(deliver(&store, &wrong_relay, 5, Uuid::new_v4(), 40, now)
        .await
        .is_err());
    assert!(deliver(&store, &d, 6, Uuid::new_v4(), 40, now + 1_001)
        .await
        .is_err());
    deliver(&store, &d, 7, Uuid::new_v4(), 40, now)
        .await
        .unwrap();
    store
        .revoke_delegation(d.installation_id, RELAY_HEX, 2, assertion(1, 2, None, now))
        .await
        .unwrap();
    store
        .revoke_delegation(d.installation_id, RELAY_HEX, 2, assertion(2, 3, None, now))
        .await
        .unwrap();
    assert!(deliver(&store, &d, 8, Uuid::new_v4(), 40, now)
        .await
        .is_err());
    let newer = delegation([9; 32], RELAY_HEX, 4, now);
    // A lost issuance response cannot reconstruct the hash's preimage; retry
    // learns the current version, then signs a strictly newer issuance.
    assert_eq!(
        store
            .upsert_delegation(newer.clone(), assertion(3, 4, Some(2), now))
            .await,
        Err(AuthorityError::StaleGeneration(3))
    );
    assert_eq!(
        store
            .installation(d.installation_id, now)
            .await
            .unwrap()
            .assertion_counter,
        3,
        "failed mutation must roll back assertion counter"
    );
    store
        .upsert_delegation(newer.clone(), assertion(3, 5, Some(3), now))
        .await
        .unwrap();
    assert!(
        store
            .upsert_delegation(
                delegation([10; 32], RELAY_HEX, 5, now),
                assertion(3, 6, Some(4), now)
            )
            .await
            .is_err(),
        "stale counter cannot mutate permission"
    );
    deliver(&store, &newer, 11, Uuid::new_v4(), 40, now)
        .await
        .unwrap();
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn fixed_relay_shares_survive_renewal_and_leave_quota_rejections_unconsumed() {
    let (pool, schema) = full_schema(4).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    let first = delegation([1; 32], RELAY_HEX, 2, now);
    let second = delegation([2; 32], &"22".repeat(32), 1, now);
    store
        .upsert_delegation(first.clone(), assertion(0, 1, Some(1), now))
        .await
        .unwrap();
    store
        .upsert_delegation(second.clone(), assertion(1, 2, Some(0), now))
        .await
        .unwrap();
    for event in [1, 2] {
        deliver(&store, &first, event, Uuid::new_v4(), 8, now)
            .await
            .unwrap();
    }
    let request = Uuid::new_v4();
    assert!(matches!(
        deliver(&store, &first, 3, request, 8, now).await,
        Err(AuthorityError::DeliveryRateLimited(1..=60))
    ));
    deliver(&store, &second, 4, Uuid::new_v4(), 8, now)
        .await
        .unwrap();
    let renewed = delegation([3; 32], RELAY_HEX, 3, now);
    store
        .upsert_delegation(renewed.clone(), assertion(2, 3, Some(2), now))
        .await
        .unwrap();
    assert!(
        matches!(
            deliver(&store, &renewed, 3, request, 8, now).await,
            Err(AuthorityError::DeliveryRateLimited(1..=60))
        ),
        "renewing a capability does not reset quota"
    );
    deliver(&store, &renewed, 3, request, 8, now + 60)
        .await
        .unwrap();
    // A quota rejection reserved neither replay identifier.
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM push_gateway_delivery_auth_replays WHERE auth_event_id=$1",
    )
    .bind(vec![3_u8; 32])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn consumed_revoked_slots_cannot_be_reallocated_within_the_window() {
    let (pool, schema) = full_schema(4).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    let first = delegation([1; 32], RELAY_HEX, 2, now);
    store
        .upsert_delegation(first.clone(), assertion(0, 1, Some(1), now))
        .await
        .unwrap();
    deliver(&store, &first, 1, Uuid::new_v4(), 8, now)
        .await
        .unwrap();
    for key in 2..5 {
        store
            .upsert_delegation(
                delegation([key; 32], &hex::encode([key; 32]), 1, now),
                assertion(key as u32 - 1, key as u32, Some(0), now),
            )
            .await
            .unwrap();
    }
    store
        .revoke_delegation(
            first.installation_id,
            RELAY_HEX,
            2,
            assertion(4, 5, None, now),
        )
        .await
        .unwrap();
    let fifth = delegation([5; 32], &"55".repeat(32), 1, now);
    assert_eq!(
        store
            .upsert_delegation(fifth.clone(), assertion(5, 6, Some(0), now))
            .await,
        Err(AuthorityError::DeliveryRateLimited(60))
    );
    store
        .upsert_delegation(fifth, assertion(5, 7, Some(0), now + 60))
        .await
        .unwrap();
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn concurrent_renewal_revocation_and_delivery_preserve_authority_fences() {
    let (pool, schema) = full_schema(4).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    let d = delegation([1; 32], RELAY_HEX, 2, now);
    let (renew, revoke) = tokio::join!(
        store.upsert_delegation(d, assertion(0, 1, Some(1), now)),
        store.revoke_delegation(Uuid::from_u128(1), RELAY_HEX, 1, assertion(0, 2, None, now))
    );
    assert_eq!(
        usize::from(renew.is_ok()) + usize::from(revoke.is_ok()),
        1,
        "counter and permission mutation commit together"
    );
    let counter = store
        .installation(Uuid::from_u128(1), now)
        .await
        .unwrap()
        .assertion_counter;
    let generation: i64 = sqlx::query_scalar("SELECT generation FROM push_gateway_delegations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(generation, 2);
    assert_eq!(counter, if renew.is_ok() { 1 } else { 2 });
    // Regardless of who won, no new legacy delivery can pass the version fence.
    assert!(admit(&store, &"22".repeat(32), Uuid::new_v4())
        .await
        .is_err());
    if renew.is_ok() {
        let d = delegation([1; 32], RELAY_HEX, 2, now);
        let (delivery, revocation) = tokio::join!(
            deliver(&store, &d, 3, Uuid::new_v4(), 40, now),
            store.revoke_delegation(d.installation_id, RELAY_HEX, 2, assertion(1, 3, None, now))
        );
        revocation.unwrap();
        // A previously admitted send may finish; revocation gates new admissions.
        drop(delivery);
        assert!(deliver(&store, &d, 4, Uuid::new_v4(), 40, now)
            .await
            .is_err());
    }
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn delegation_catalog_matches_desired_state_after_expansion() {
    let (pool, schema) = full_schema(1).await;
    let migrated:Vec<(String,String,bool)>=sqlx::query_as("SELECT attname::text,format_type(atttypid,atttypmod),attnotnull FROM pg_attribute WHERE attrelid='push_gateway_delegations'::regclass AND attnum>0 AND NOT attisdropped ORDER BY attname").fetch_all(&pool).await.unwrap();
    sqlx::raw_sql("ALTER TABLE push_gateway_delegations RENAME TO migrated_delegations")
        .execute(&pool)
        .await
        .unwrap();
    // Existing index names follow the renamed table, so compare columns after
    // creating the declarative table under its own name.
    let table = include_str!("../../../../schema/tables/public/push_gateway_delegations.sql");
    sqlx::raw_sql(table.split("CREATE INDEX").next().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let desired:Vec<(String,String,bool)>=sqlx::query_as("SELECT attname::text,format_type(atttypid,atttypmod),attnotnull FROM pg_attribute WHERE attrelid='push_gateway_delegations'::regclass AND attnum>0 AND NOT attisdropped ORDER BY attname").fetch_all(&pool).await.unwrap();
    assert_eq!(migrated, desired);
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn endpoint_rotation_response_loss_retries_the_exact_target_only() {
    let (pool, schema) = full_schema(2).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let id = Uuid::from_u128(1);
    let now = Utc::now().timestamp();
    store
        .rotate_endpoint(id, 1, 2, vec![9], [9; 32], assertion(0, 1, None, now))
        .await
        .unwrap();
    store
        .rotate_endpoint(id, 1, 2, vec![10], [9; 32], assertion(1, 2, None, now))
        .await
        .unwrap();
    assert_eq!(
        store
            .rotate_endpoint(id, 1, 2, vec![11], [11; 32], assertion(2, 3, None, now))
            .await,
        Err(AuthorityError::Rejected)
    );
    let installation = store.installation(id, now).await.unwrap();
    assert_eq!(installation.assertion_counter, 2);
    assert_eq!(installation.token_fingerprint, [9; 32]);
    assert_eq!(
        installation.token_ciphertext,
        vec![9],
        "exact retry does not overwrite the original committed token"
    );
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn startup_allocation_guard_preserves_excess_legacy_authority() {
    let (pool, schema) = full_schema(2).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    store.verify_relay_allocations(now).await.unwrap();
    for key in 2_u8..=5 {
        // Old writers could approve more relays, including not-yet-active grants.
        sqlx::query("INSERT INTO push_gateway_delegations(id,installation_id,relay_pubkey,endpoint_epoch,generation,not_before,expires_at) VALUES($1,$2,$3,1,1,$4,$5)")
            .bind(Uuid::from_u128(100 + u128::from(key)))
            .bind(Uuid::from_u128(1)).bind(vec![key;32])
            .bind(at(now+86_400).unwrap()).bind(at(now+604_800).unwrap())
            .execute(&pool).await.unwrap();
        if key == 4 {
            store.verify_relay_allocations(now).await.unwrap();
        }
    }
    assert_eq!(
        store.verify_relay_allocations(now).await,
        Err(AuthorityError::Unavailable)
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM push_gateway_delegations WHERE revoked_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        live, 5,
        "startup refusal must not delete or revoke existing authority"
    );
    assert_eq!(
        store
            .installation(Uuid::from_u128(1), now)
            .await
            .unwrap()
            .assertion_counter,
        0
    );
    store
        .revoke_delegation(Uuid::from_u128(1), &hex::encode([5; 32]), 1, None)
        .await
        .unwrap();
    store.verify_relay_allocations(now).await.unwrap();
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn maximum_generation_retains_revocation_capacity_for_opaque_and_legacy_grants() {
    let (pool, schema) = full_schema(2).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    let d = delegation([17; 32], RELAY_HEX, i64::MAX, now);
    store
        .upsert_delegation(d.clone(), assertion(0, 1, Some(1), now))
        .await
        .unwrap();
    deliver(&store, &d, 18, Uuid::new_v4(), 40, now)
        .await
        .unwrap();
    for (previous, next) in [(1, 2), (2, 3)] {
        store
            .revoke_delegation(
                d.installation_id,
                RELAY_HEX,
                i64::MAX,
                assertion(previous, next, None, now),
            )
            .await
            .unwrap();
    }
    assert!(deliver(&store, &d, 19, Uuid::new_v4(), 40, now)
        .await
        .is_err());
    assert_eq!(
        store
            .upsert_delegation(d, assertion(3, 4, Some(i64::MAX), now))
            .await,
        Err(AuthorityError::StaleGeneration(i64::MAX))
    );
    let mut legacy = delegation([20; 32], &"22".repeat(32), i64::MAX, now);
    legacy.grant_hash = None;
    store.upsert_delegation(legacy.clone(), None).await.unwrap();
    for _ in 0..2 {
        store
            .revoke_delegation(legacy.installation_id, &legacy.relay_pubkey, i64::MAX, None)
            .await
            .unwrap();
    }
    assert!(
        store.upsert_delegation(legacy, None).await.is_err(),
        "legacy renewal cannot restore revoked authority"
    );
    pool.close().await;
    drop_schema(&schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL"]
async fn lock_wait_cannot_extend_delivery_or_signed_management_expiry() {
    let (pool, schema) = full_schema(5).await;
    install_authority(&pool).await;
    let store = PostgresAuthorityStore::new(pool.clone());
    let now = Utc::now().timestamp();
    let d = delegation([71;32], RELAY_HEX, 2, now);
    store.upsert_delegation(d.clone(), assertion(0,1,Some(1),now)).await.unwrap();
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM push_gateway_installations WHERE id=$1 FOR UPDATE").bind(d.installation_id).fetch_one(&mut *lock).await.unwrap();
    let delivery_store=store.clone(); let target=d.clone(); let request=Uuid::new_v4();
    let pending_delivery=tokio::spawn(async move {
        delivery_store.authorize_delivery(target.id,target.grant_hash,RELAY_HEX,1,2,&"72".repeat(32),request,now+1,60,40,now).await
    });
    let management_store=store.clone();
    let mut signed=assertion(1,2,Some(2),now).unwrap(); signed.request_expires_at=Some(now+1);
    let pending_management=tokio::spawn(async move {
        management_store.upsert_delegation(delegation([73;32],RELAY_HEX,3,now),Some(signed)).await
    });
    // Authority locks outlive initially valid signed request expiries.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    lock.commit().await.unwrap();
    assert!(matches!(pending_delivery.await.unwrap(),Err(AuthorityError::Rejected)));
    assert_eq!(pending_management.await.unwrap(),Err(AuthorityError::Rejected));
    assert_eq!(store.installation(d.installation_id,Utc::now().timestamp()).await.unwrap().assertion_counter,1);
    let replays:i64=sqlx::query_scalar("SELECT count(*) FROM push_gateway_delivery_request_replays WHERE request_id=$1").bind(request).fetch_one(&pool).await.unwrap();
    assert_eq!(replays,0);
    pool.close().await; drop_schema(&schema).await;
}
