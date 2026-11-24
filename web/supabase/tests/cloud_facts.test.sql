-- Database tests for web/supabase/migrations/20260909000113_cloud_facts_publication.sql.
--
-- These run real statements as the real roles (anon, authenticated,
-- service_role) against a real PostgreSQL 15+ database; nothing here inspects
-- SQL text. pgTAP is not required — failures raise, and ON_ERROR_STOP aborts.
--
--   createdb cw_facts_test
--   psql -v ON_ERROR_STOP=1 -d cw_facts_test -f web/supabase/tests/cloud_facts.test.sql
--
-- Run it against a disposable local database only: it creates the anon /
-- authenticated / service_role roles when they are missing (service_role needs
-- BYPASSRLS, as on Supabase, which needs a superuser connection), applies the
-- migration, and ends with ROLLBACK so the database is left untouched.
--
-- The base64 blobs below are syntactically valid placeholders, not keys and
-- not signatures: the database never verifies a signature, clients do, against
-- keys pinned in the binary and in web/lib/cloud-facts/keys.ts.

\set ON_ERROR_STOP on

begin;

-- ---------------------------------------------------------------------------
-- Roles and schema under test
-- ---------------------------------------------------------------------------

do $roles$
begin
  if not exists (select 1 from pg_roles where rolname = 'anon') then
    create role anon nologin noinherit;
  end if;
  if not exists (select 1 from pg_roles where rolname = 'authenticated') then
    create role authenticated nologin noinherit;
  end if;
  if not exists (select 1 from pg_roles where rolname = 'service_role') then
    create role service_role nologin noinherit bypassrls;
  end if;
  if not (select rolbypassrls from pg_roles where rolname = 'service_role') then
    raise exception 'service_role must have BYPASSRLS for the publisher path to work';
  end if;
  execute format('grant anon, authenticated, service_role to %I', current_user);
end
$roles$;

\ir ../migrations/20260909000113_cloud_facts_publication.sql
\ir ../migrations/20261117000114_registry_channel_publication.sql

-- ---------------------------------------------------------------------------
-- Fixture helpers (rolled back with everything else)
-- ---------------------------------------------------------------------------

create function public.tst_ts(t timestamptz) returns text
language sql as $$
  select to_char(t at time zone 'utc', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
$$;

-- The shape web/scripts/facts-publish.mjs signs, minus the parts the database
-- does not constrain.
create function public.tst_payload(
  p_channel text, p_version bigint, p_published timestamptz,
  p_not_after timestamptz default null, p_applies text default '*'
) returns jsonb
language sql as $$
  select jsonb_strip_nulls(jsonb_build_object(
           'schema_version', 1,
           'channel', p_channel,
           'facts_version', p_version,
           'published_at', public.tst_ts(p_published),
           'not_after', public.tst_ts(p_not_after),
           'applies_to', p_applies))
         || jsonb_build_object('models', '[]'::jsonb, 'provider_defaults', '{}'::jsonb,
                               'release', null, 'announcements', '[]'::jsonb)
$$;

-- encode(..., 'base64') wraps at 76 columns; the publisher never emits newlines
-- and neither may a hand-written insert.
create function public.tst_b64(p jsonb) returns text
language sql as $$
  select translate(encode(convert_to(p::text, 'utf8'), 'base64'), E'\n', '')
$$;

create function public.tst_release(
  p_slug text, p_version bigint,
  p_published timestamptz default null, p_not_after timestamptz default null,
  p_applies text default '*', p_key text default 'cwf-test-registry',
  p_payload jsonb default null, p_b64 text default null,
  p_sig text default null, p_sigs jsonb default '[]'::jsonb
) returns void
language plpgsql as $$
declare
  pub timestamptz := date_trunc('second', coalesce(p_published, now() - interval '1 minute'));
  expires timestamptz := date_trunc('second', p_not_after);
  payload jsonb := coalesce(p_payload, public.tst_payload(p_slug, p_version, pub, expires, p_applies));
  inserted int;
begin
  insert into public.facts_release (
    channel_id, facts_version, schema_version, envelope_version, applies_to, key_id,
    payload_b64, sig_b64, sigs, payload, published_at, not_after, published_by, notes)
  select c.id, p_version, 1, 1, p_applies, p_key,
         coalesce(p_b64, public.tst_b64(payload)), coalesce(p_sig, repeat('A', 86) || '=='),
         p_sigs, payload, pub, expires, 'operator@example.test', 'internal note'
    from public.facts_channel c
   where c.scope = 'global' and c.slug = p_slug;
  get diagnostics inserted = row_count;
  if inserted <> 1 then
    raise exception 'fixture channel % is missing', p_slug;
  end if;
end
$$;

-- ---------------------------------------------------------------------------
-- Fixtures, written the way the publisher writes them: as service_role
-- ---------------------------------------------------------------------------

do $fixtures$
begin
  set local role service_role;
  insert into public.facts_key (key_id, scope, algorithm, public_key, status)
    values ('cwf-test-registry', 'global', 'ed25519', repeat('A', 43) || '=', 'active');
  insert into public.facts_channel (scope, slug, visibility) values
    ('global', 'stable', 'public'),
    ('global', 'beta', 'private'),
    ('global', 'future', 'public'),
    ('global', 'expired', 'public'),
    ('global', 'rollback', 'public');
  perform public.tst_release('stable', 7);
  perform public.tst_release('beta', 3);
  perform public.tst_release('future', 2, now() + interval '1 hour');
  perform public.tst_release('expired', 4, now() - interval '2 hours', now() - interval '1 hour');
  perform public.tst_release('rollback', 1);
  perform public.tst_release('rollback', 2);
  reset role;
end
$fixtures$;

-- ---------------------------------------------------------------------------
-- 1. The publishable-key read path returns exactly one verifiable row
-- ---------------------------------------------------------------------------

do $published_read$
declare
  rows_seen int;
  v_release_id uuid; v_facts_version bigint;
  v_schema_version int; v_envelope_version int; v_applies_to text; v_key_id text;
  v_payload_b64 text; v_sig_b64 text; v_sigs jsonb; v_payload_sha256 text;
  v_published_at timestamptz; v_not_after timestamptz;
  v_payload jsonb;
begin
  set local role anon;
  -- Exactly the query web/lib/cloud-facts.ts sends over PostgREST.
  select count(*) into rows_seen
    from public.facts_current f where f.channel = 'stable' and f.scope = 'global';
  select f.release_id, f.facts_version, f.schema_version,
         f.envelope_version, f.applies_to, f.key_id, f.payload_b64, f.sig_b64,
         f.sigs, f.payload_sha256, f.published_at, f.not_after
    into v_release_id, v_facts_version, v_schema_version,
         v_envelope_version, v_applies_to, v_key_id, v_payload_b64, v_sig_b64,
         v_sigs, v_payload_sha256, v_published_at, v_not_after
    from public.facts_current f
   where f.channel = 'stable' and f.scope = 'global'
   limit 1;
  reset role;

  if rows_seen <> 1 then
    raise exception 'expected exactly 1 current row for stable, got %', rows_seen;
  end if;
  if v_facts_version <> 7 or v_schema_version <> 1 or v_envelope_version <> 1
     or v_applies_to <> '*' or v_key_id <> 'cwf-test-registry' or v_sigs <> '[]'::jsonb
     or v_release_id is null or v_not_after is not null or v_published_at is null then
    raise exception 'facts_current row does not carry the published contract: v=% schema=% envelope=% applies=% key=% sigs=%',
      v_facts_version, v_schema_version, v_envelope_version, v_applies_to, v_key_id, v_sigs;
  end if;
  -- The digest is derived from the decoded signed bytes, not supplied.
  if v_payload_sha256 is distinct from encode(sha256(decode(v_payload_b64, 'base64')), 'hex')
     or v_payload_sha256 !~ '^[a-f0-9]{64}$' then
    raise exception 'payload_sha256 % does not hash the decoded payload', v_payload_sha256;
  end if;
  v_payload := convert_from(decode(v_payload_b64, 'base64'), 'utf8')::jsonb;
  if v_payload -> 'channel' <> to_jsonb('stable'::text)
     or v_payload -> 'facts_version' <> to_jsonb(7::bigint)
     or (v_payload ->> 'published_at')::timestamptz <> v_published_at then
    raise exception 'signed payload disagrees with the row metadata';
  end if;
  if v_sig_b64 !~ '^[A-Za-z0-9+/]{86}==$' then
    raise exception 'envelope columns are not well formed';
  end if;
  raise notice 'ok 1: facts_current serves one verifiable head row to anon';
end
$published_read$;

-- ---------------------------------------------------------------------------
-- 2. anon and authenticated cannot write anything, or read operator columns
-- ---------------------------------------------------------------------------

do $write_refusals$
declare
  role_name text;
  stmt text;
  refused boolean;
  releases_before bigint;
  releases_after bigint;
begin
  select count(*) into releases_before from public.facts_release;
  foreach role_name in array array['anon', 'authenticated'] loop
    foreach stmt in array array[
      'insert into public.facts_channel (scope, slug, visibility) values (''global'', ''rogue'', ''public'')',
      'update public.facts_channel set visibility = ''public''',
      'delete from public.facts_channel',
      'insert into public.facts_key (key_id, public_key) values (''cwf-rogue'', ''x'')',
      'update public.facts_key set status = ''active''',
      'select public_key from public.facts_key',
      'insert into public.facts_release (channel_id, facts_version, applies_to, key_id, payload_b64, sig_b64, payload, published_at) values (gen_random_uuid(), 99, ''*'', ''cwf-test-registry'', ''e30='', ''x'', ''{}''::jsonb, now())',
      'update public.facts_release set status = ''revoked''',
      'delete from public.facts_release',
      'select payload from public.facts_release',
      'select published_by from public.facts_release',
      'select notes from public.facts_release',
      'select revoke_reason from public.facts_release',
      'select created_at from public.facts_channel'
    ] loop
      execute format('set local role %I', role_name);
      refused := false;
      begin
        execute stmt;
      exception when insufficient_privilege then
        refused := true;
      end;
      reset role;
      if not refused then
        raise exception 'role % was not refused: %', role_name, stmt;
      end if;
    end loop;

    -- The read view is not a write surface either.
    execute format('set local role %I', role_name);
    refused := false;
    begin
      execute 'insert into public.facts_current (channel, facts_version) values (''stable'', 99)';
    exception when others then
      refused := true;
    end;
    reset role;
    if not refused then
      raise exception 'role % was allowed to insert into facts_current', role_name;
    end if;
  end loop;

  select count(*) into releases_after from public.facts_release;
  if releases_before <> releases_after then
    raise exception 'refused writes still changed facts_release (% -> %)', releases_before, releases_after;
  end if;
  raise notice 'ok 2: anon and authenticated are refused every write and every operator column';
end
$write_refusals$;

-- ---------------------------------------------------------------------------
-- 3. Private, future-dated and expired releases never reach the public read
-- ---------------------------------------------------------------------------

do $exclusions$
declare
  hidden_channel text;
  anon_rows int;
  service_rows int;
  anon_channels int;
  anon_beta_releases int;
begin
  foreach hidden_channel in array array['beta', 'future', 'expired'] loop
    set local role anon;
    select count(*) into anon_rows from public.facts_current where channel = hidden_channel;
    reset role;
    set local role service_role;
    select count(*) into service_rows from public.facts_current where channel = hidden_channel;
    reset role;
    if anon_rows <> 0 then
      raise exception 'channel % leaked % row(s) to anon', hidden_channel, anon_rows;
    end if;
    -- service_role bypasses RLS, so this proves the view predicate itself.
    if service_rows <> 0 then
      raise exception 'channel % leaked % row(s) through the view predicate', hidden_channel, service_rows;
    end if;
  end loop;

  set local role anon;
  select count(*) into anon_channels from public.facts_channel where slug = 'beta';
  select count(*) into anon_beta_releases from public.facts_release where facts_version = 3;
  reset role;
  if anon_channels <> 0 or anon_beta_releases <> 0 then
    raise exception 'private channel is visible on the base tables: % channel row(s), % release row(s)',
      anon_channels, anon_beta_releases;
  end if;
  raise notice 'ok 3: private, future-dated and expired releases are excluded from both surfaces';
end
$exclusions$;

-- ---------------------------------------------------------------------------
-- 4. Revoking the head does not silently roll back to an older version
-- ---------------------------------------------------------------------------

do $revocation$
declare
  served bigint;
  rows_seen int;
  refused_code text;
begin
  set local role anon;
  select facts_version into served from public.facts_current where channel = 'rollback';
  reset role;
  if served is distinct from 2::bigint then
    raise exception 'expected the head version 2 before revocation, got %', coalesce(served::text, 'no row');
  end if;

  set local role service_role;
  update public.facts_release r
     set status = 'revoked', revoked_at = now(), revoke_reason = 'fixture revocation'
    from public.facts_channel c
   where c.id = r.channel_id and c.slug = 'rollback' and r.facts_version = 2;
  reset role;

  set local role anon;
  select count(*) into rows_seen from public.facts_current where channel = 'rollback';
  reset role;
  if rows_seen <> 0 then
    raise exception 'revoking the head served % row(s); version 1 must not come back', rows_seen;
  end if;

  -- The supported repair is a higher version, not un-revocation or mutation.
  set local role service_role;
  refused_code := null;
  begin
    update public.facts_release r
       set status = 'published', revoked_at = null, revoke_reason = null
      from public.facts_channel c
     where c.id = r.channel_id and c.slug = 'rollback' and r.facts_version = 2;
  exception when others then refused_code := sqlstate;
  end;
  if refused_code is distinct from '23001' then
    raise exception 'un-revoking a release was not refused (sqlstate %)', coalesce(refused_code, 'none');
  end if;

  refused_code := null;
  begin
    update public.facts_release r
       set payload_b64 = 'QUFBQQ=='
      from public.facts_channel c
     where c.id = r.channel_id and c.slug = 'rollback' and r.facts_version = 1;
  exception when others then refused_code := sqlstate;
  end;
  if refused_code is distinct from '23001' then
    raise exception 'mutating a signed column was not refused (sqlstate %)', coalesce(refused_code, 'none');
  end if;

  perform public.tst_release('rollback', 3);
  reset role;

  set local role anon;
  select count(*) into rows_seen from public.facts_current where channel = 'rollback';
  select facts_version into served from public.facts_current where channel = 'rollback';
  reset role;
  if rows_seen <> 1 or served is distinct from 3::bigint then
    raise exception 'republishing above the revoked head served % row(s) at version %',
      rows_seen, coalesce(served::text, 'none');
  end if;
  raise notice 'ok 4: a revoked head blocks delivery, stays revoked, and is repaired by a higher version';
end
$revocation$;

-- ---------------------------------------------------------------------------
-- 5. Versions are unique and monotonic per channel; the read stays one row
-- ---------------------------------------------------------------------------

do $monotonic$
declare
  code text;
  rows_seen int;
  served bigint;
  stale bigint;
begin
  set local role service_role;
  foreach stale in array array[3::bigint, 7::bigint] loop
    code := null;
    begin
      perform public.tst_release('stable', stale);
    exception when others then code := sqlstate;
    end;
    if code is distinct from '23505' then
      raise exception 'republishing stable version % was not refused (sqlstate %)', stale, coalesce(code, 'none');
    end if;
  end loop;
  perform public.tst_release('stable', 8);
  reset role;

  set local role anon;
  select count(*), max(facts_version) into rows_seen, served
    from public.facts_current where channel = 'stable' and scope = 'global';
  reset role;
  if rows_seen <> 1 or served is distinct from 8::bigint then
    raise exception 'expected one row at version 8 after publishing, got % row(s) at %',
      rows_seen, coalesce(served::text, 'none');
  end if;
  raise notice 'ok 5: facts_version is unique and monotonic, and the read stays deterministic at one row';
end
$monotonic$;

-- ---------------------------------------------------------------------------
-- 6. Rows whose metadata, bytes, size or signatures do not hold are rejected
-- ---------------------------------------------------------------------------

do $consistency$
declare
  pub timestamptz := date_trunc('second', now() - interval '1 minute');
  code text;
  rows_seen int;
  served bigint;
  field text;
  candidate jsonb;
begin
  set local role service_role;

  -- The signed payload's channel must be the channel it is published on.
  code := null;
  begin
    perform public.tst_release('stable', 20, p_published => pub,
                               p_payload => public.tst_payload('beta', 20, pub));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'cross-channel payload accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- Outer facts_version must repeat the signed one.
  code := null;
  begin
    perform public.tst_release('stable', 21, p_published => pub,
                               p_payload => public.tst_payload('stable', 22, pub));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'facts_version mismatch accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- SQL CHECK accepts NULL unless the full predicate is explicitly true.
  -- Missing signed metadata must not pass through three-valued SQL logic.
  foreach field in array array['facts_version', 'schema_version', 'applies_to', 'published_at'] loop
    for candidate in
      select public.tst_payload('stable', 22, pub) - field
      union all
      select public.tst_payload('stable', 22, pub) || jsonb_build_object(field, null)
    loop
      code := null;
      begin
        perform public.tst_release('stable', 22, p_published => pub, p_payload => candidate);
      exception when others then code := sqlstate;
      end;
      if code is distinct from '23514' then
        raise exception 'missing/null signed % accepted (sqlstate %)', field, coalesce(code, 'none');
      end if;
    end loop;
  end loop;

  -- payload must be the decode of payload_b64.
  code := null;
  begin
    perform public.tst_release('stable', 23, p_published => pub,
                               p_b64 => public.tst_b64(public.tst_payload('stable', 23, pub, null, '>=1.0.0')));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'payload divergent from the signed bytes accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- Newline-wrapped base64 (what encode() produces) is not canonical.
  code := null;
  begin
    perform public.tst_release('stable', 24, p_published => pub,
                               p_b64 => encode(convert_to(public.tst_payload('stable', 24, pub)::text, 'utf8'), 'base64'));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'newline-wrapped payload_b64 accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- Bounded size: a self-consistent payload above MAX_PAYLOAD_BYTES.
  code := null;
  begin
    perform public.tst_release('stable', 25, p_published => pub,
      p_payload => public.tst_payload('stable', 25, pub) || jsonb_build_object('pad', repeat('x', 600000)));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'oversized payload_b64 accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- A signature must at least be 64 bytes of canonical base64.
  code := null;
  begin
    perform public.tst_release('stable', 26, p_published => pub, p_sig => repeat('A', 40));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'malformed sig_b64 accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- Extra rotation signatures: bounded count and well-formed elements.
  code := null;
  begin
    perform public.tst_release('stable', 27, p_published => pub,
      p_sigs => (select jsonb_agg(jsonb_build_object('key_id', 'cwf-rotation', 'sig_b64', repeat('A', 86) || '=='))
                   from generate_series(1, 8)));
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'more than seven extra signatures accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  code := null;
  begin
    perform public.tst_release('stable', 28, p_published => pub,
                               p_sigs => '[{"key_id": "bad id", "sig_b64": "AAAA"}]'::jsonb);
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'malformed extra signature accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- Expiry must follow publication.
  code := null;
  begin
    perform public.tst_release('stable', 29, p_published => pub, p_not_after => pub - interval '1 hour');
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'not_after before published_at accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- An unregistered key cannot be referenced.
  code := null;
  begin
    perform public.tst_release('stable', 30, p_published => pub, p_key => 'cwf-not-registered');
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23503' then
    raise exception 'unregistered key_id accepted (sqlstate %)', coalesce(code, 'none');
  end if;

  -- Only global-scope channels may be public.
  code := null;
  begin
    insert into public.facts_channel (scope, slug, visibility) values ('org-acme', 'stable', 'public');
  exception when others then code := sqlstate;
  end;
  if code is distinct from '23514' then
    raise exception 'a non-global channel was made public (sqlstate %)', coalesce(code, 'none');
  end if;
  reset role;

  -- None of the rejected inserts moved the channel high-water mark.
  set local role anon;
  select count(*), max(facts_version) into rows_seen, served
    from public.facts_current where channel = 'stable' and scope = 'global';
  reset role;
  if rows_seen <> 1 or served is distinct from 8::bigint then
    raise exception 'rejected publications disturbed the channel: % row(s) at version %',
      rows_seen, coalesce(served::text, 'none');
  end if;
  raise notice 'ok 6: metadata, byte, size, signature, key and scope constraints all hold';
end
$consistency$;

-- ---------------------------------------------------------------------------
-- 7. The exposed surface is exactly the contract (no wildcard exposure)
-- ---------------------------------------------------------------------------

do $surface$
declare
  view_columns text[];
  expected text[] := array[
    'channel', 'scope', 'release_id', 'facts_version', 'schema_version',
    'envelope_version', 'applies_to', 'key_id', 'payload_b64', 'sig_b64',
    'sigs', 'payload_sha256', 'published_at', 'not_after'];
  reader_role text;
  hidden text;
  visible text;
  guarded text;
  options text[];
begin
  select array_agg(column_name::text order by ordinal_position) into view_columns
    from information_schema.columns
   where table_schema = 'public' and table_name = 'facts_current';
  if view_columns is distinct from expected then
    raise exception 'facts_current exposes % but the reader contract is %', view_columns, expected;
  end if;

  select c.reloptions into options
    from pg_class c join pg_namespace n on n.oid = c.relnamespace
   where n.nspname = 'public' and c.relname = 'facts_current' and c.relkind = 'v';
  if options is null or not ('security_invoker=true' = any(options)) then
    raise exception 'facts_current is not a security_invoker view (reloptions %)', options;
  end if;

  foreach guarded in array array['facts_channel', 'facts_key', 'facts_release'] loop
    if not (select relrowsecurity from pg_class c join pg_namespace n on n.oid = c.relnamespace
             where n.nspname = 'public' and c.relname = guarded) then
      raise exception 'row level security is not enabled on public.%', guarded;
    end if;
  end loop;

  foreach reader_role in array array['anon', 'authenticated'] loop
    if not has_table_privilege(reader_role, 'public.facts_current', 'select') then
      raise exception 'role % cannot read facts_current', reader_role;
    end if;
    if has_table_privilege(reader_role, 'public.facts_key', 'select') then
      raise exception 'role % can read the key registry', reader_role;
    end if;
    foreach guarded in array array['public.facts_channel', 'public.facts_key', 'public.facts_release', 'public.facts_current'] loop
      if has_table_privilege(reader_role, guarded, 'insert')
         or has_table_privilege(reader_role, guarded, 'update')
         or has_table_privilege(reader_role, guarded, 'delete') then
        raise exception 'role % holds a write privilege on %', reader_role, guarded;
      end if;
    end loop;
    foreach hidden in array array['payload', 'published_by', 'notes', 'revoked_at', 'revoke_reason', 'created_at'] loop
      if has_column_privilege(reader_role, 'public.facts_release', hidden, 'select') then
        raise exception 'role % can read operator column facts_release.%', reader_role, hidden;
      end if;
    end loop;
    foreach visible in array array['payload_b64', 'sig_b64', 'sigs', 'payload_sha256', 'published_at', 'not_after'] loop
      if not has_column_privilege(reader_role, 'public.facts_release', visible, 'select') then
        raise exception 'role % cannot read published column facts_release.%', reader_role, visible;
      end if;
    end loop;
  end loop;

  if not has_table_privilege('service_role', 'public.facts_release', 'insert')
     or not has_table_privilege('service_role', 'public.facts_release', 'update')
     or not has_table_privilege('service_role', 'public.facts_key', 'insert') then
    raise exception 'the publisher role cannot publish';
  end if;
  if has_table_privilege('service_role', 'public.facts_release', 'delete') then
    raise exception 'the publisher role can delete publication history';
  end if;
  raise notice 'ok 7: grants, policies and the view surface match the delivery contract';
end
$surface$;

do $registry_channel$
declare
  pub timestamptz := date_trunc('second', now() - interval '1 minute');
  reg_payload jsonb;
  large_reg_payload jsonb;
  rows_seen int;
  served_version bigint;
  rejected boolean;
begin
  set local role service_role;
  insert into public.facts_channel (scope, slug, visibility)
    values ('global', 'registry', 'public');

  reg_payload := jsonb_build_object(
    'schema', 'codewhale.registry.v1',
    'revision', 1,
    'content_hash', 'sha256:' || repeat('a', 64),
    'generated_at', public.tst_ts(pub),
    'providers', '[]'::jsonb,
    'models', '[]'::jsonb,
    'offerings', '[]'::jsonb,
    'dialects', '{}'::jsonb
  );
  perform public.tst_release('registry', 1, pub, null, '*', 'cwf-test-registry', reg_payload);

  large_reg_payload := reg_payload || jsonb_build_object(
    'revision', 2,
    'engine_compat', jsonb_build_object('pad', repeat('x', 540000))
  );
  perform public.tst_release('registry', 2, pub, pub + interval '1 day', '*', 'cwf-test-registry',
    large_reg_payload || jsonb_build_object('not_after', public.tst_ts(pub + interval '1 day')));

  rejected := false;
  begin
    perform public.tst_release('stable', 99, pub, null, '*', 'cwf-test-registry',
      reg_payload || jsonb_build_object('revision', 99));
  exception when check_violation then
    rejected := true;
  end;
  if not rejected then
    raise exception 'stable channel accepted a codewhale.registry.v1 payload';
  end if;

  rejected := false;
  begin
    perform public.tst_release('registry', 3, pub, null, '*', 'cwf-test-registry',
      public.tst_payload('registry', 3, pub));
  exception when check_violation then
    rejected := true;
  end;
  if not rejected then
    raise exception 'registry channel accepted a non-registry facts payload';
  end if;

  rejected := false;
  begin
    perform public.tst_release('registry', 3, pub, null, '>=99.0.0', 'cwf-test-registry',
      reg_payload || jsonb_build_object('revision', 3));
  exception when check_violation then
    rejected := true;
  end;
  if not rejected then
    raise exception 'registry channel accepted tampered outer applies_to';
  end if;
  reset role;

  set local role anon;
  select count(*), max(f.facts_version) into rows_seen, served_version
    from public.facts_current f where f.channel = 'registry' and f.scope = 'global';
  reset role;
  if rows_seen <> 1 or served_version <> 2 then
    raise exception 'expected registry head v2 for anon, saw % row(s) at %', rows_seen, served_version;
  end if;
  raise notice 'ok 8: registry channel accepts >512 KiB codewhale.registry.v1 releases and isolates channel schemas';
end
$registry_channel$;

select 'cloud facts storage: all assertions passed' as result;

rollback;
