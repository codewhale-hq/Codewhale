alter table public.facts_release
  drop constraint if exists facts_release_payload_b64_check;

alter table public.facts_release
  add constraint facts_release_payload_b64_check check (
    octet_length(payload_b64) between 4 and 11184812
    and octet_length(payload_b64) % 4 = 0
    and payload_b64 ~ '^[A-Za-z0-9+/]+={0,2}$'
    and (
      case
        when payload ->> 'schema' = 'codewhale.registry.v1' then octet_length(decode(payload_b64, 'base64')) <= 8388608
        else octet_length(payload_b64) <= 699052 and octet_length(decode(payload_b64, 'base64')) <= 524288
      end
    )
  );

alter table public.facts_release
  drop constraint if exists facts_release_payload_meta_check;

alter table public.facts_release
  add constraint facts_release_payload_meta_check check (
    case
      when payload ->> 'schema' = 'codewhale.registry.v1' then (
        schema_version = 1
        and payload -> 'revision' = to_jsonb(facts_version)
        and (payload -> 'applies_to' is null or jsonb_typeof(payload -> 'applies_to') = 'string')
        and coalesce(payload ->> 'applies_to', '*') = applies_to
        and payload ->> 'generated_at' ~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$'
        and (payload ->> 'generated_at')::timestamptz = published_at
        and jsonb_typeof(payload -> 'providers') = 'array'
        and jsonb_typeof(payload -> 'models') = 'array'
        and jsonb_typeof(payload -> 'offerings') = 'array'
        and jsonb_typeof(payload -> 'dialects') = 'object'
        and case
              when not_after is null then payload ->> 'not_after' is null
              else payload ->> 'not_after' ~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$'
                   and (payload ->> 'not_after')::timestamptz = not_after
            end
      ) is true
      else (
        payload -> 'facts_version' = to_jsonb(facts_version)
        and payload -> 'schema_version' = to_jsonb(schema_version)
        and payload -> 'applies_to' = to_jsonb(applies_to)
        and payload ->> 'published_at' ~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$'
        and (payload ->> 'published_at')::timestamptz = published_at
        and case
              when not_after is null then payload ->> 'not_after' is null
              else payload ->> 'not_after' ~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$'
                   and (payload ->> 'not_after')::timestamptz = not_after
            end
      ) is true
    end
  );

create or replace function public.facts_release_guard()
returns trigger
language plpgsql
security invoker
set search_path = ''
as $facts_release_guard$
declare
  channel_row public.facts_channel%rowtype;
  extra_sig jsonb;
begin
  if tg_op = 'UPDATE' then
    if new.id is distinct from old.id
       or new.channel_id is distinct from old.channel_id
       or new.facts_version is distinct from old.facts_version
       or new.schema_version is distinct from old.schema_version
       or new.envelope_version is distinct from old.envelope_version
       or new.applies_to is distinct from old.applies_to
       or new.key_id is distinct from old.key_id
       or new.payload_b64 is distinct from old.payload_b64
       or new.sig_b64 is distinct from old.sig_b64
       or new.sigs is distinct from old.sigs
       or new.payload is distinct from old.payload
       or new.published_at is distinct from old.published_at
       or new.not_after is distinct from old.not_after then
      raise exception 'signed columns of facts_release % are immutable; publish a higher facts_version instead', old.id
        using errcode = 'restrict_violation';
    end if;
    if old.status = 'revoked' and new.status is distinct from 'revoked' then
      raise exception 'facts_release % cannot be un-revoked; publish a higher facts_version instead', old.id
        using errcode = 'restrict_violation';
    end if;
    return new;
  end if;

  select * into channel_row from public.facts_channel where id = new.channel_id for update;
  if not found then
    raise exception 'facts_channel % does not exist', new.channel_id
      using errcode = 'foreign_key_violation';
  end if;

  if channel_row.slug = 'registry' then
    if new.payload ->> 'schema' is distinct from 'codewhale.registry.v1' then
      raise exception 'registry channel requires schema codewhale.registry.v1, got %',
        coalesce(new.payload ->> 'schema', '<missing>')
        using errcode = 'check_violation';
    end if;
  else
    if new.payload ->> 'schema' is not null or new.payload -> 'channel' is distinct from to_jsonb(channel_row.slug) then
      raise exception 'signed payload channel % does not match channel %',
        coalesce(new.payload ->> 'channel', '<missing>'), channel_row.slug
        using errcode = 'check_violation';
    end if;
  end if;

  if new.facts_version <= channel_row.max_facts_version then
    raise exception 'facts_version % is not above the published high-water mark % for channel %',
      new.facts_version, channel_row.max_facts_version, channel_row.slug
      using errcode = 'unique_violation';
  end if;

  for extra_sig in select value from jsonb_array_elements(new.sigs) loop
    if jsonb_typeof(extra_sig) <> 'object'
       or coalesce(extra_sig ->> 'key_id', '') !~ '^cwf-[a-z0-9-]{1,32}$'
       or coalesce(extra_sig ->> 'sig_b64', '') !~ '^[A-Za-z0-9+/]{86}==$' then
      raise exception 'extra signature % is not a well-formed {key_id, sig_b64} pair', extra_sig
        using errcode = 'check_violation';
    end if;
  end loop;

  update public.facts_channel set max_facts_version = new.facts_version where id = new.channel_id;
  return new;
end;
$facts_release_guard$;

notify pgrst, 'reload schema';
