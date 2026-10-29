# CueFounder Buzz edition

CueFounder self-hosts a fork of [Buzz](https://github.com/block/buzz) for its
founder-support community at founders.cuefounder.ai.

This first edition preserves the pinned upstream relay engine and adds a
CueFounder title, icon, and open-source attribution to the bundled web/invite
shell. The companion membership application handles founder onboarding,
profile drafting, company checks, and operator approval. Those companion
features are not represented as changes to the Buzz engine.

Further community updates will be developed on this fork. Native clients still
use the Buzz identity/membership protocol; full browser chat and native SSO
are not shipped by this edition.

Build from this directory: `docker build -t cuefounder-buzz:2026.10.28.1 .`.
The base relay image is pinned in Dockerfile. Review upstream changes and test
invite redemption, membership gating, and chat before changing that digest.
