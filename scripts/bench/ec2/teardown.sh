#!/usr/bin/env bash
# Delete everything provision.sh created, after checking its tags, and keep
# before/after describe output in $STATE_DIR/teardown/ as proof.
#
# Usage: teardown.sh STATE_DIR
set -euo pipefail

STATE_DIR="${1:?usage: teardown.sh STATE_DIR}"
RES="$STATE_DIR/aws-resources.json"
OUT="$STATE_DIR/teardown"
mkdir -p "$OUT"
get() { python3 -c "import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])" "$RES" "$1"; }
REGION="$(get region)"; PROFILE="$(get profile)"
INSTANCE_ID="$(get instance_id)"; VOLUME_ID="$(get root_volume_id)"; ENI_ID="$(get network_interface_id)"
SG_ID="$(get security_group_id)"; KEY_NAME="$(get key_name)"; KEY_FILE="$(get key_file)"
aws() { command aws --profile "$PROFILE" --region "$REGION" "$@"; }

aws ec2 describe-instances --instance-ids "$INSTANCE_ID" --output json > "$OUT/before-instance.json"
python3 - "$OUT/before-instance.json" <<'PY'
import json, sys
inst = json.load(open(sys.argv[1]))["Reservations"][0]["Instances"][0]
tags = {t["Key"]: t["Value"] for t in inst.get("Tags", [])}
if tags.get("Project") != "quicksilver-bench" or tags.get("Owner") != "jonathan":
    raise SystemExit(f"refusing to terminate an instance with tags {tags}")
PY

if [[ "$(aws ec2 describe-instances --instance-ids "$INSTANCE_ID" --query 'Reservations[0].Instances[0].State.Name' --output text)" != terminated ]]; then
  aws ec2 terminate-instances --instance-ids "$INSTANCE_ID" --output json > "$OUT/terminate-instance.json"
  aws ec2 wait instance-terminated --instance-ids "$INSTANCE_ID"
fi
for _ in $(seq 36); do
  state="$(aws ec2 describe-volumes --volume-ids "$VOLUME_ID" --query 'Volumes[0].State' --output text 2>/dev/null || echo gone)"
  [[ "$state" == gone || "$state" == deleted ]] && break
  [[ "$state" == available ]] && aws ec2 delete-volume --volume-id "$VOLUME_ID" || true
  sleep 10
done
for _ in $(seq 36); do
  state="$(aws ec2 describe-network-interfaces --network-interface-ids "$ENI_ID" --query 'NetworkInterfaces[0].Status' --output text 2>/dev/null || echo gone)"
  [[ "$state" == gone ]] && break
  [[ "$state" == available ]] && aws ec2 delete-network-interface --network-interface-id "$ENI_ID" || true
  sleep 10
done
for _ in $(seq 20); do aws ec2 delete-security-group --group-id "$SG_ID" 2>/dev/null && break; sleep 6; done
aws ec2 delete-key-pair --key-name "$KEY_NAME" >/dev/null
rm -f "$KEY_FILE"

{
  echo "instance: $(aws ec2 describe-instances --instance-ids "$INSTANCE_ID" --query 'Reservations[0].Instances[0].State.Name' --output text 2>&1)"
  echo "volume: $(aws ec2 describe-volumes --volume-ids "$VOLUME_ID" --output text 2>&1 | head -1)"
  echo "eni: $(aws ec2 describe-network-interfaces --network-interface-ids "$ENI_ID" --output text 2>&1 | head -1)"
  echo "security group: $(aws ec2 describe-security-groups --group-ids "$SG_ID" --output text 2>&1 | head -1)"
  echo "key pair: $(aws ec2 describe-key-pairs --key-names "$KEY_NAME" --output text 2>&1 | head -1)"
  echo "local key file present: $([[ -e "$KEY_FILE" ]] && echo yes || echo no)"
} | tee "$OUT/after.txt"
