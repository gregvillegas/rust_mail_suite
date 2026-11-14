-- Shared enum types and helper functions used by later migrations.

CREATE TYPE user_role AS ENUM ('super_admin', 'domain_admin', 'user');

CREATE TYPE account_status AS ENUM ('active', 'suspended', 'disabled');

CREATE TYPE audit_outcome AS ENUM ('success', 'failure');

-- Keeps `updated_at` current on every UPDATE. Attached per table via trigger.
CREATE FUNCTION set_updated_at() RETURNS trigger AS $$
BEGIN
    NEW.updated_at = now();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
