---
status: accepted
date: 2026-11-08
decision-makers: ocpu (maintainer)
---

# Support systems without NetworkManager through wpa_supplicant's control socket

## Context and Problem Statement

Raspberry Pi OS 11 and older run dhcpcd, which starts wpa_supplicant for
`wlan0`; NetworkManager is not used. Switching such a system to
NetworkManager changes its whole network setup. Can the daemon work with the
system as installed?

## Considered Options

* Require NetworkManager.
* Use wpa_supplicant's D-Bus API.
* Use wpa_supplicant's control socket, the one `wpa_cli` uses.

## Decision Outcome

Chosen option: "The control socket", because dhcpcd's wpa_supplicant runs
without D-Bus (no `-u` option) but with a control socket, opened to a group by
`ctrl_interface=... GROUP=netdev` in the standard configuration.

### Consequences

* Good, because the daemon works on the system as installed; the address
  comes from dhcpcd, and joined networks are saved with `SAVE_CONFIG`.
* Good, because `--backend auto` chooses NetworkManager when it runs, so one
  binary serves both kinds of system.
* Bad, because the control-socket protocol is text, parsed by this code; it
  is tested against a stand-in socket and the wpa_supplicant of Raspbian 11
only.

### Confirmation

On a Pi Zero W with Raspbian 11 as installed: a failed join left nothing
behind, a join was saved to `wpa_supplicant.conf`, and the network was
rejoined after a reboot with NetworkManager off.

## More Information

The systemd unit used to have `Wants=NetworkManager.service`, which started
NetworkManager on such systems and, through its `Conflicts=dhcpcd.service`,
stopped dhcpcd. The unit now only orders itself after NetworkManager.
