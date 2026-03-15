---
name: Bug Report
about: Report a problem with facelock
labels: bug
---

## Description

A clear description of the bug.

## Steps to reproduce

1.
2.
3.

## Expected behavior

What you expected to happen.

## Actual behavior

What actually happened. Include error messages or log output if available.

## Environment

- **OS/distro**: (e.g. Arch Linux, Ubuntu 24.04)
- **Kernel**: (`uname -r`)
- **Camera**: (e.g. `/dev/video2`, IR or RGB)
- **Facelock version**: (`facelock --version`)
- **Install method**: (cargo, AUR, .deb, .rpm)

## Logs

<details>
<summary>Daemon logs</summary>

```
journalctl -u facelock-daemon --no-pager -n 50
```

</details>

<details>
<summary>Facelock status</summary>

```
sudo facelock status
```

</details>
