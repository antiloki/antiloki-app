# Security policy

antiloki runs agents that can read files and run commands on your machine, so security reports matter.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability reporting for this repository (Security tab, "Report a vulnerability"). Include what you found, how to reproduce it, and the version.

We will acknowledge a report within a few days and keep you updated until it is fixed.

## What is in scope

- the local HTTP backend (`serve`) and its guards: origin and host checks, approval flows;
- the MCP server and hooks (`agent`);
- anything that lets one pane, team or specialist act outside the scope it was given;
- the desktop shell and the updater.

## Supported versions

Only the latest release receives fixes while antiloki is pre-1.0.
