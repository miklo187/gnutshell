# Security policy

gnutshell runs your login session: it draws the lock screen, asks for passwords
for polkit and decides which programs may use some of its D-Bus services. It
has **not been security audited**.

## Reporting a problem

Please report security problems privately, not in a public issue:
[report a vulnerability](https://github.com/miklo187/gnutshell/security/advisories/new)
(or use **Report a vulnerability** on the repository's **Security** page). You
need a GitHub account.

It helps to include:

- the gnutshell version or commit, your distribution and your mutter version;
- what an attacker needs to have already, such as a program running in your
  session, a sandboxed app or physical access;
- the steps or a small program that shows the problem;
- what the attacker gains.

Please don't include real passwords or other secrets. Don't test lock screen
passwords by script: repeated wrong ones can lock your account.

This is a hobby project, so there is no promised response time.

## Supported versions

Fixes go into the newest release and `master`. Older releases are not updated.

## Known limits

The README's [Caveats](README.md#caveats) describe limits that are already
known, among them that programs in your session can claim other apps' names in
notifications, and that gnutshell trusts the owners of certain D-Bus names, as
gnome-shell does.

Whatever an unsandboxed program of yours can do without gnutshell, such as
reading your files, is not a gnutshell vulnerability by itself. What gnutshell
lets it do beyond that, such as reading the screen, is.
