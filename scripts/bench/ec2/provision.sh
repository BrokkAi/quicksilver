#!/usr/bin/env bash
# Launch one idle benchmark VM for the cargo-quicksilver vs cargo-mutants
# comparison. Every resource is new, tagged, and recorded in
# $STATE_DIR/aws-resources.json for teardown.sh. A failed launch removes
# whatever it created. The instance terminates itself after 12 hours.
#
# Usage: provision.sh STATE_DIR
set -euo pipefail

STATE_DIR="${1:?usage: provision.sh STATE_DIR}"
mkdir -p "$STATE_DIR"
REGION=us-east-1
PROFILE=default
INSTANCE_TYPE="${INSTANCE_TYPE:-c7i.16xlarge}"
VPC_ID=vpc-0050fb351b91024e5
SUBNET_ID="${SUBNET_ID:-subnet-0566e17e33f57c69f}"
NAME_TAG=quicksilver-bench
OWNER_TAG=jonathan
SUFFIX="$(date -u +%Y%m%dT%H%M%SZ)"
KEY_NAME="${NAME_TAG}-${SUFFIX}"
SG_NAME="${NAME_TAG}-${SUFFIX}"
KEY_FILE="$STATE_DIR/${KEY_NAME}.pem"
aws() { command aws --profile "$PROFILE" --region "$REGION" "$@"; }

ACCOUNT="$(aws sts get-caller-identity --query Account --output text)"
[[ "$ACCOUNT" == 842609633142 ]] || { echo "refusing: account $ACCOUNT is not 842609633142" >&2; exit 2; }
HOST_CIDR="$(curl -fsS https://checkip.amazonaws.com | tr -d '[:space:]')/32"
AMI_ID="$(aws ssm get-parameter --name /aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id --query Parameter.Value --output text)"

SG_ID= INSTANCE_ID= KEEP=0
cleanup_partial() {
  local status=$?
  if [[ $status -ne 0 && $KEEP -ne 1 ]]; then
    set +e
    [[ -n "$INSTANCE_ID" ]] && aws ec2 terminate-instances --instance-ids "$INSTANCE_ID" >/dev/null \
      && aws ec2 wait instance-terminated --instance-ids "$INSTANCE_ID"
    if [[ -n "$SG_ID" ]]; then
      for _ in $(seq 10); do aws ec2 delete-security-group --group-id "$SG_ID" 2>/dev/null && break; sleep 6; done
    fi
    aws ec2 delete-key-pair --key-name "$KEY_NAME" >/dev/null 2>&1
    rm -f "$KEY_FILE"
  fi
  return $status
}
trap cleanup_partial EXIT

TAGS="{Key=Name,Value=${NAME_TAG}},{Key=Owner,Value=${OWNER_TAG}},{Key=Project,Value=${NAME_TAG}}"
aws ec2 create-key-pair --key-name "$KEY_NAME" --key-type ed25519 \
  --tag-specifications "ResourceType=key-pair,Tags=[${TAGS}]" \
  --query KeyMaterial --output text > "$KEY_FILE"
chmod 600 "$KEY_FILE"
SG_ID="$(aws ec2 create-security-group --group-name "$SG_NAME" \
  --description 'Temporary SSH for the quicksilver benchmark' --vpc-id "$VPC_ID" \
  --tag-specifications "ResourceType=security-group,Tags=[${TAGS}]" --query GroupId --output text)"
aws ec2 authorize-security-group-ingress --group-id "$SG_ID" \
  --ip-permissions "[{\"IpProtocol\":\"tcp\",\"FromPort\":22,\"ToPort\":22,\"IpRanges\":[{\"CidrIp\":\"${HOST_CIDR}\"}]}]" >/dev/null

USER_DATA="$(printf '#!/bin/bash\nshutdown -h +720\n' | base64 -w0)"
INSTANCE_ID="$(aws ec2 run-instances --image-id "$AMI_ID" --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" \
  --instance-initiated-shutdown-behavior terminate \
  --user-data "$USER_DATA" \
  --network-interfaces "[{\"DeviceIndex\":0,\"SubnetId\":\"${SUBNET_ID}\",\"Groups\":[\"${SG_ID}\"],\"AssociatePublicIpAddress\":true,\"DeleteOnTermination\":true}]" \
  --block-device-mappings '[{"DeviceName":"/dev/sda1","Ebs":{"VolumeSize":300,"VolumeType":"gp3","Iops":12000,"Throughput":1000,"Encrypted":true,"DeleteOnTermination":true}}]' \
  --metadata-options '{"HttpEndpoint":"enabled","HttpTokens":"required"}' \
  --tag-specifications "ResourceType=instance,Tags=[${TAGS}]" "ResourceType=volume,Tags=[${TAGS}]" \
  --count 1 --query 'Instances[0].InstanceId' --output text)"
aws ec2 wait instance-running --instance-ids "$INSTANCE_ID"
read -r PUBLIC_IP VOLUME_ID ENI_ID < <(aws ec2 describe-instances --instance-ids "$INSTANCE_ID" \
  --query 'Reservations[0].Instances[0].[PublicIpAddress,BlockDeviceMappings[0].Ebs.VolumeId,NetworkInterfaces[0].NetworkInterfaceId]' \
  --output text)
aws ec2 create-tags --resources "$ENI_ID" --tags "Key=Name,Value=${NAME_TAG}" "Key=Owner,Value=${OWNER_TAG}" "Key=Project,Value=${NAME_TAG}"

python3 - "$STATE_DIR/aws-resources.json" <<PY
import json, sys
json.dump({
    "created_at": "$SUFFIX", "region": "$REGION", "profile": "$PROFILE", "account": "$ACCOUNT",
    "instance_type": "$INSTANCE_TYPE", "ami_id": "$AMI_ID", "ssh_source_cidr": "$HOST_CIDR",
    "key_name": "$KEY_NAME", "key_file": "$KEY_FILE", "security_group_id": "$SG_ID",
    "instance_id": "$INSTANCE_ID", "root_volume_id": "$VOLUME_ID", "network_interface_id": "$ENI_ID",
    "public_ip": "$PUBLIC_IP", "tags": {"Name": "$NAME_TAG", "Owner": "$OWNER_TAG", "Project": "$NAME_TAG"},
}, open(sys.argv[1], "w"), indent=2, sort_keys=True)
PY
KEEP=1
echo "instance $INSTANCE_ID at $PUBLIC_IP; ssh -i $KEY_FILE ubuntu@$PUBLIC_IP"
