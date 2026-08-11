# HOT provider validation runbook

Run the offline capability probe first. For an authorized staging provider,
run the live probe and then a tiny pack test covering upload, direct GET,
range/resume, URL refresh, corruption rejection, and delete. Verify API logs
contain resolver requests but no pack-byte proxy traffic.

Use a synthetic pack with a unique BLAKE3 identity. Delete only the HOT copy
after recording its COLD location, request it through the pack resolver, wait
for `restore_pending` to clear, and verify the restored HOT pack and extracted
logical chunk. Keep the legacy chunk resolver test in the same run so a client
can fall back during rollout.
