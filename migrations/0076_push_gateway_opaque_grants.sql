SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';

-- Gateway-owned tables still duplicated in the relay schema; expand both.
ALTER TABLE push_gateway_delegations ADD COLUMN grant_hash BYTEA;
