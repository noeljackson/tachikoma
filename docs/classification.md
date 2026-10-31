# Activity classification and authorization

Network assessment asks what kind of activity the observed context most likely
represents. It does not establish that the executable, destination or connection
is safe, or that the owner authorized it. Human approval remains a separate step.

Jev first estimates activity and launch context, then the overall category. A
cached/build executable, project/worktree working directory and local destination
can jointly support development, even with only timeout/shell ancestry. Missing
verified checksums, listener ownership or complete provenance do not erase that
workload pattern. Development includes migrations and local services; explicit
test context is needed to call something a test.

A generic runtime, a cache path alone, a familiar hostname or loopback alone can
remain unknown. All connections originate on this machine; the destination IP is
what identifies a loopback connection. Dependency lifecycle markers, credential
scanners and unexpected child callbacks outweigh reassuring project/cache names.
Reported metadata can be misleading. These classifications never create grants.

`unknown` is an explicit answer in each question. Its score is the provider's
confidence in selecting that answer, not confidence in an identified activity,
measured detection accuracy or a probability of safety. Scores from activity,
origin and category are separate opinions and cannot be combined into proof.

## Model-only comparisons

The evaluator accepts a queue manifest or assessment profile and a bounded case
set. It has no approval/firewall connection and records no decisions. By default
it compares baseline, context and staged input variants. Use
`--evaluation-variants staged` for a focused comparison between profile versions.

```sh
docker build --target assessment-eval --tag tachikoma:assessment-eval .
# Supply only a private Jev credential and the bounded dataset/profile to the
# container; mount no approval credentials, broker sockets or firewall state.
# Worker arguments:
# --jev-key-file /private/jev.key --evaluate /inputs/cases.json \
# --evaluation-profile /inputs/profile.json --evaluation-variants staged \
# --report /reports/results.json
```

`conformance/assessment/network-cases.json` contains authored development,
ordinary, ambiguous and dependency-callback regression cases. Reference examples
have distinct IDs. These labels test rubric behavior, not held-out real-world
security accuracy. Historical allows and unknown results are not safety labels.
Reports preserve provider failures in totals and contain no raw input facts or
credentials. Existing historical assessments retain their original profile;
newly submitted requests use the current queue profile.

## 24 October 2026 comparison

The previous profile matched 26/30 authored regression labels. The activity-focused
profile matched 30/30, including all six synthetic dependency-attack cases; the
unattributed scanner and other ambiguous controls remained unknown. The separately
replayed, unlabeled cache/worktree/loopback request changed from unknown to
activity development, origin project_development, category development.

These are rubric regression results, not detection accuracy or evidence that a
connection is safe. Reference examples were refined against the regression set.
The provider returned 1.0 scores for the replayed development pattern; these are
model estimates, not measured certainty. See
[`activity-results-20261024.json`](../conformance/assessment/activity-results-20261024.json)
for profile hashes, results, failures and limitations. The replay inputs remain
private; published results omit host paths and raw evidence.
