# Security

nutsh holds Prism Central credentials. It stores a password in the operating system keyring
where there is one and in a file under `$XDG_STATE_HOME/nutsh/secrets/` where there is not, and
it carries that password, or the session cookie it bought, on every request it makes. A defect
in how it handles either is worth reporting privately.

## Reporting a vulnerability

Open a private advisory through GitHub: **Security → Report a vulnerability** on this
repository. That reaches the maintainer without the report being public first. If the advisory
form is unavailable, open an issue asking for a private channel and say nothing else in it.

Please include what you need to reproduce it: the version (`nutsh --version`), what you ran,
what you expected and what happened instead. Never paste a real password, session cookie,
`Authorization` header or the address of a production Prism Central into a report. If a log is
useful, `NUTSH_LOG=debug` is written to be safe to share; say so if you find that it is not,
because that is itself the bug.

You should get an acknowledgement within a week. This is one person's project with no paid
support behind it, so there is no fix deadline to promise. What you will get is a straight
answer about whether it is a defect, and credit in the release notes if you want it.

## What counts

- A credential, a session cookie or an `Authorization` header reaching a log, the cache, the
  configuration file, a fixture, `argv`, or a child process's environment.
- A session or a credential being sent anywhere other than the Prism Central it was obtained
  from, including across a redirect.
- Certificate verification being skipped anywhere `--insecure` was not asked for.
- A read-only session (`--readonly`) or a guardrail reaching the wire with a mutating request.
- A crash reachable from what a Prism Central sends back, since that is data this program does
  not control.

## What does not

- `--insecure`, which does what it says and is documented as doing it.
- Anything that needs write access to the user's own configuration or state directory. A
  program cannot defend against whoever already owns the files it reads.
- The vendored OpenAPI documents under `specs/`. They are published by Nutanix; report anything
  wrong with them to Nutanix.
