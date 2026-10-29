# Buzz push notes

Push is experimental. These notes capture current decisions; we will write a
stable specification after validating the design through launch and use.
[NIP-PL](nips/NIP-PL.md) is background, not the current Buzz contract.

- Relays advertise `buzz-push-v1`, never `nip-pl`. Updated clients require it.
- Compatible changes keep the capability name. Incompatible behavior that
  clients must distinguish gets a new version.
- Each gateway serves one server-configured Apple application and APNs transport.
  Enrollment, transcripts, grants, leases and discovery have no application-profile
  selector. The relay advertises transport classes in `class_support`.
- The earlier single-application initialization requires an empty pre-launch
  authority store. The opaque-grant upgrade preserves initialized installations,
  active authority, assertion counters and revocation tombstones.
- Publish the matching gateway image and chart, update deployment configuration,
  then enable the new relay capability. Verify enrollment, delivery, renewal and
  revocation on a physical device before production activation.

## Gateway grants and management

The gateway keeps application-encrypted APNs tokens and App Attest installation
keys. Enrollment alone uses a one-use public challenge. New management requests
bind the gateway origin, operation, targets, parameters, versions and a short
expiry in their App Attest assertion. The counter and permission mutation commit
together. TLS, fixed reconnect contents and relay NIP-98 body/path/time/signature
verification remain required.

Each relay authorization returns a fresh 256-bit random grant. Only its SHA-256
hash and authoritative permission state are stored. Renewal replaces the old
grant; response loss requires newer-generation reissuance, with durable client
handoff/cleanup state retained until completion. Revocation blocks new admission,
but cannot recall work already admitted.

Opaque grants are compatible with existing relay lease storage. Gateway replicas
must deploy read support before any issues the new format. Legacy encrypted
grants and challenge-based management have an explicit bounded transition;
APNs token encryption is permanent. See [deployment guidance](push-gateway-deployment.md)
for the reader/writer rollout, legacy deadline, storage re-encryption command and
key-retirement evidence.

## Channel identifier grammar

Relays advertise `h_grammar: "uuid-v4-lowercase"`. Mobile also accepts
`uuid-lowercase`, which drops the UUID version and variant restriction so
UUIDv5 starter channels can be subscribed. Against a `uuid-v4-lowercase` relay,
mobile checks positive and ignore channel filters before enrollment or
publication. It keeps the desired filters and rechecks relay capabilities every
five minutes rather than sending a lease the relay would reject.

Do not change the relay advertisement until mobile builds that only parse
`uuid-v4-lowercase` have drained. Those builds parse the descriptor before
renewal, opt-out and revocation, so a new grammar would leave their existing
leases active until expiry.
