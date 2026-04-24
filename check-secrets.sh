#!/usr/bin/env bash
# Pre-commit hook: blocks commits containing secrets, tokens, or vendor files.
# Install: cp scripts/check-secrets.sh .git/hooks/pre-commit && chmod +x .git/hooks/pre-commit

set -euo pipefail

RED='\033[0;31m'
NC='\033[0m'
BLOCKED=0

# Files staged for commit
STAGED=$(git diff --cached --name-only --diff-filter=ACM 2>/dev/null || true)

if [ -z "$STAGED" ]; then
    exit 0
fi

# 1. Block forbidden file patterns
FORBIDDEN_FILES=(
    '\.env$'
    '\.env\.'
    '\.pem$'
    '\.key$'
    '\.p12$'
    '\.pfx$'
    '\.jks$'
    'credentials\.json'
    'service.account\.json'
    'gcloud.*\.json'
    '\.secret'
    'id_rsa'
    'id_ed25519'
    '\.keystore$'
    'token\.json'
)

for pattern in "${FORBIDDEN_FILES[@]}"; do
    MATCHES=$(echo "$STAGED" | grep -iE "$pattern" || true)
    if [ -n "$MATCHES" ]; then
        echo -e "${RED}BLOCKED: Secret/key file staged:${NC}"
        echo "$MATCHES"
        BLOCKED=1
    fi
done

# 2. Block vendor/dependency directories
VENDOR_DIRS=(
    '^node_modules/'
    '^vendor/'
    '^\.pnpm-store/'
    '^target/'
    '^dist/'
    '^build/'
    '/__pycache__/'
    '\.pyc$'
)

for pattern in "${VENDOR_DIRS[@]}"; do
    MATCHES=$(echo "$STAGED" | grep -E "$pattern" || true)
    if [ -n "$MATCHES" ]; then
        echo -e "${RED}BLOCKED: Vendor/build files staged:${NC}"
        echo "$MATCHES" | head -5
        echo "(and possibly more)"
        BLOCKED=1
    fi
done

# 3. Scan file contents for secrets
SECRET_PATTERNS=(
    'PRIVATE KEY'
    'AWS_SECRET_ACCESS_KEY'
    'AWS_ACCESS_KEY_ID'
    'sk_live_'
    'sk_test_'
    'pk_live_'
    'ghp_[a-zA-Z0-9]{36}'
    'gho_[a-zA-Z0-9]{36}'
    'github_pat_'
    'xox[bps]-'
    'AKIA[0-9A-Z]{16}'
    'password\s*=\s*["\x27][^"\x27]{8,}'
    'secret\s*=\s*["\x27][^"\x27]{8,}'
    'token\s*=\s*["\x27][^"\x27]{8,}'
    'api_key\s*=\s*["\x27][^"\x27]{8,}'
    'DATABASE_URL=postgres://[^@]+:[^@]+@'
)

for file in $STAGED; do
    # Skip binary files and lock files
    if [[ "$file" == *.lock ]] || [[ "$file" == *.png ]] || [[ "$file" == *.jpg ]] || [[ "$file" == *.woff2 ]]; then
        continue
    fi

    # Only check if file exists (might be deleted)
    if [ ! -f "$file" ]; then
        continue
    fi

    for pattern in "${SECRET_PATTERNS[@]}"; do
        MATCH=$(grep -nEi "$pattern" "$file" 2>/dev/null | head -3 || true)
        if [ -n "$MATCH" ]; then
            # Allow patterns in the check-secrets script itself, test files, examples, and design reference files
            if [[ "$file" == *"check-secrets"* ]] || [[ "$file" == *"test"* ]] || [[ "$file" == *".example"* ]] || [[ "$file" == *"design/"* ]] || [[ "$file" == *".jsx" ]] || [[ "$file" == *".html" ]]; then
                continue
            fi
            echo -e "${RED}BLOCKED: Possible secret in $file:${NC}"
            echo "$MATCH"
            BLOCKED=1
        fi
    done
done

if [ "$BLOCKED" -eq 1 ]; then
    echo ""
    echo -e "${RED}Commit blocked. Remove secrets/vendor files before committing.${NC}"
    echo "If this is a false positive, review the file and use --no-verify (with caution)."
    exit 1
fi

exit 0
