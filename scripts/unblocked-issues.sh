#!/usr/bin/env bash
# List open issues whose `Blocked by:` issues are all closed (or none).
set -euo pipefail

gh issue list --state all --limit 200 --json number,state,title,body |
    jq -r '
        (reduce .[] as $i ({}; .[$i.number|tostring] = $i.state)) as $state
        | .[]
        | select(.state == "OPEN")
        | . as $issue
        | (
            ($issue.body // "")
            | [ scan("(?im)^[-\\s]*Blocked by:\\s*(.*)$") ]
            | first // [""]
            | .[0]
          ) as $line
        | ( [ $line | scan("\\d+") | tonumber ] ) as $blockers
        | select( all($blockers[]; ($state[.|tostring] // "CLOSED") == "CLOSED") )
        | "#\(.number)\t\(.title)"
    ' |
    sort -n -k1.2
