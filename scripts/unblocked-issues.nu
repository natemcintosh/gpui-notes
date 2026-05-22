#!/usr/bin/env nu
# List open issues whose `Blocked by:` issues are all closed (or none).

let issues = (
  gh issue list --state all --limit 200 --json number,state,title,body
  | from json
)

let state_of = (
  $issues
  | reduce --fold {} {|it, acc|
      $acc | insert ($it.number | into string) $it.state
    }
)

$issues
| where state == "OPEN"
| where {|issue|
    let line = (
      $issue.body
      | default ""
      | lines
      | where {|l| $l =~ '(?i)^[-\s]*Blocked by:'}
      | get 0?
      | default ""
    )
    let blockers = (
      $line
      | parse --regex '(?<n>\d+)'
      | get n
      | each { into int }
    )
    $blockers | all {|b|
      ($state_of | get -o ($b | into string) | default "CLOSED") == "CLOSED"
    }
  }
| sort-by number
| each {|i| $"#($i.number)\t($i.title)" }
| to text
