#!/usr/bin/env bash
# =============================================================================
# SchemaForge Demo Data Seed Script
# =============================================================================
#
# Populates a running SchemaForge server with realistic demo entities across
# all 14 schemas defined in schemas/demo.schema.
#
# Usage:
#   BASE_URL=http://127.0.0.1:3000 bash scripts/seed-demo-data.sh
#
# Prerequisites:
#   - Server running with demo.schema loaded
#   - curl and jq installed

set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:3000}"
API="${BASE_URL}/api/v1/forge/schemas"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

TOTAL=0
ERRORS=0

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# create_entity SCHEMA JSON_BODY
#   POSTs to the entity API, prints status to stderr, echoes entity ID to stdout.
create_entity() {
    local schema="$1"
    local body="$2"

    local response
    local http_code
    local tmp
    tmp=$(mktemp)

    http_code=$(curl -s -o "$tmp" -w '%{http_code}' \
        -X POST \
        -H "Content-Type: application/json" \
        -d "$body" \
        "${API}/${schema}/entities")

    TOTAL=$((TOTAL + 1))

    if [ "$http_code" = "201" ]; then
        local entity_id
        entity_id=$(jq -r '.id' "$tmp")
        echo -e "  ${GREEN}+${NC} ${schema}: ${entity_id}" >&2
        rm -f "$tmp"
        echo "$entity_id"
    else
        ERRORS=$((ERRORS + 1))
        echo -e "  ${RED}x${NC} ${schema}: HTTP ${http_code}" >&2
        jq -r '.message // .details // .' "$tmp" 2>/dev/null | head -3 | sed 's/^/    /' >&2
        rm -f "$tmp"
        echo "ERROR"
    fi
}

wait_for_server() {
    echo -e "${CYAN}Waiting for server at ${BASE_URL}...${NC}"
    for i in $(seq 1 30); do
        if curl -sf "${BASE_URL}/health" > /dev/null 2>&1; then
            echo -e "${GREEN}Server is ready.${NC}"
            return 0
        fi
        sleep 1
    done
    echo -e "${RED}ERROR: Server not reachable after 30 seconds.${NC}"
    exit 1
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

wait_for_server

echo ""
echo -e "${BOLD}Seeding demo data...${NC}"
echo ""

# =========================================================================
# Layer 1: Tags (no dependencies)
# =========================================================================
echo -e "${CYAN}--- Tags ---${NC}"

TAG1_ID=$(create_entity "Tag" "$(jq -n '{fields: {
    name: "urgent",
    color: "#e74c3c",
    category: "priority",
    description: "Requires immediate attention",
    active: true
}}')")

TAG2_ID=$(create_entity "Tag" "$(jq -n '{fields: {
    name: "frontend",
    color: "#3498db",
    category: "skill",
    description: "Frontend development work",
    active: true
}}')")

TAG3_ID=$(create_entity "Tag" "$(jq -n '{fields: {
    name: "backend",
    color: "#2ecc71",
    category: "skill",
    description: "Backend development work",
    active: true
}}')")

TAG4_ID=$(create_entity "Tag" "$(jq -n '{fields: {
    name: "devops",
    color: "#9b59b6",
    category: "department",
    description: "DevOps and infrastructure",
    active: true
}}')")

TAG5_ID=$(create_entity "Tag" "$(jq -n '{fields: {
    name: "healthcare",
    color: "#1abc9c",
    category: "industry",
    description: "Healthcare industry vertical",
    active: true
}}')")

# =========================================================================
# Layer 2: Organizations (no dependencies)
# =========================================================================
echo -e "${CYAN}--- Organizations ---${NC}"

ORG1_ID=$(create_entity "Organization" "$(jq -n '{fields: {
    name: "Acme Corporation",
    slug: "acme-corp",
    billing_email: "billing@acme-corp.io",
    plan: "business",
    max_seats: 50,
    logo_url: "https://example.com/acme-logo.png",
    settings: {theme: "dark", timezone: "America/New_York"},
    founded: "2018-03-15T00:00:00Z",
    active: true,
    owner_id: "system"
}}')")

ORG2_ID=$(create_entity "Organization" "$(jq -n '{fields: {
    name: "Globex Industries",
    slug: "globex",
    billing_email: "accounts@globex.io",
    plan: "enterprise",
    max_seats: 200,
    logo_url: "https://example.com/globex-logo.png",
    settings: {theme: "light", timezone: "America/Los_Angeles", sso_enabled: true},
    founded: "2015-07-01T00:00:00Z",
    active: true,
    owner_id: "system"
}}')")

# =========================================================================
# Layer 3: Departments (→ Organization)
# =========================================================================
echo -e "${CYAN}--- Departments ---${NC}"

DEPT_ENG_ID=$(create_entity "Department" "$(jq -n --arg org "$ORG1_ID" '{fields: {
    name: "Engineering",
    code: "ENG",
    description: "Software engineering and architecture",
    parent_org: $org,
    budget: 2500000.00,
    headcount_limit: 30,
    active: true
}}')")

DEPT_SALES_ID=$(create_entity "Department" "$(jq -n --arg org "$ORG1_ID" '{fields: {
    name: "Sales",
    code: "SALES",
    description: "Revenue generation and client relations",
    parent_org: $org,
    budget: 1200000.00,
    headcount_limit: 15,
    active: true
}}')")

DEPT_OPS_ID=$(create_entity "Department" "$(jq -n --arg org "$ORG2_ID" '{fields: {
    name: "Operations",
    code: "OPS",
    description: "Business operations and logistics",
    parent_org: $org,
    budget: 800000.00,
    headcount_limit: 20,
    active: true
}}')")

# =========================================================================
# Layer 4: Employees (→ Department, → Employee for manager)
# =========================================================================
echo -e "${CYAN}--- Employees ---${NC}"

# CEO — no manager, no department
EMP_CEO_ID=$(create_entity "Employee" "$(jq -n '{fields: {
    full_name: "Diana Chen",
    email: "diana.chen@acme-corp.io",
    phone: "+1-555-0100",
    title: "Chief Executive Officer",
    hire_date: "2018-03-15T00:00:00Z",
    salary: 280000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["leadership", "strategy", "fundraising"],
    certifications: ["PMP", "MBA"],
    emergency_contact: {name: "Robert Chen", phone: "+1-555-0199", relationship: "spouse"},
    home_address: {street: "100 Oak Lane", city: "San Francisco", state: "CA", postal_code: "94105", country: "US"},
    owner_id: "system",
    active: true
}}')")

# VP Engineering — reports to CEO
EMP_VP_ENG_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_ENG_ID" --arg mgr "$EMP_CEO_ID" '{fields: {
    full_name: "Marcus Johnson",
    email: "marcus.johnson@acme-corp.io",
    phone: "+1-555-0101",
    title: "VP of Engineering",
    department: $dept,
    manager: $mgr,
    hire_date: "2019-01-10T00:00:00Z",
    salary: 220000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["rust", "architecture", "team-management", "distributed-systems"],
    certifications: ["AWS Solutions Architect"],
    emergency_contact: {name: "Lisa Johnson", phone: "+1-555-0191", relationship: "partner"},
    home_address: {street: "42 Maple Ave", city: "Oakland", state: "CA", postal_code: "94612", country: "US"},
    owner_id: "system",
    active: true
}}')")

# VP Sales — reports to CEO
EMP_VP_SALES_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_SALES_ID" --arg mgr "$EMP_CEO_ID" '{fields: {
    full_name: "Sarah Mitchell",
    email: "sarah.mitchell@acme-corp.io",
    phone: "+1-555-0102",
    title: "VP of Sales",
    department: $dept,
    manager: $mgr,
    hire_date: "2019-06-01T00:00:00Z",
    salary: 210000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["sales-strategy", "enterprise-sales", "negotiation", "crm"],
    emergency_contact: {name: "Tom Mitchell", phone: "+1-555-0192", relationship: "brother"},
    home_address: {street: "88 Pine St", city: "San Francisco", state: "CA", postal_code: "94111", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Senior Engineer 1 — reports to VP Eng
EMP_SR_ENG1_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_ENG_ID" --arg mgr "$EMP_VP_ENG_ID" '{fields: {
    full_name: "Alex Rivera",
    email: "alex.rivera@acme-corp.io",
    phone: "+1-555-0103",
    title: "Senior Software Engineer",
    department: $dept,
    manager: $mgr,
    hire_date: "2020-02-15T00:00:00Z",
    salary: 175000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["rust", "typescript", "react", "postgresql", "kubernetes"],
    certifications: ["CKA"],
    emergency_contact: {name: "Maria Rivera", phone: "+1-555-0193", relationship: "mother"},
    home_address: {street: "15 Cedar Blvd", city: "Berkeley", state: "CA", postal_code: "94704", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Senior Engineer 2 — reports to VP Eng
EMP_SR_ENG2_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_ENG_ID" --arg mgr "$EMP_VP_ENG_ID" '{fields: {
    full_name: "Priya Sharma",
    email: "priya.sharma@acme-corp.io",
    phone: "+1-555-0104",
    title: "Senior Backend Engineer",
    department: $dept,
    manager: $mgr,
    hire_date: "2020-08-01T00:00:00Z",
    salary: 170000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["rust", "go", "surrealdb", "graphql", "docker"],
    emergency_contact: {name: "Raj Sharma", phone: "+1-555-0194", relationship: "father"},
    home_address: {street: "200 University Ave", city: "Palo Alto", state: "CA", postal_code: "94301", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Junior Engineer — reports to Sr Eng 1
EMP_JR_ENG_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_ENG_ID" --arg mgr "$EMP_SR_ENG1_ID" '{fields: {
    full_name: "Jordan Lee",
    email: "jordan.lee@acme-corp.io",
    phone: "+1-555-0105",
    title: "Software Engineer",
    department: $dept,
    manager: $mgr,
    hire_date: "2023-09-01T00:00:00Z",
    salary: 120000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["typescript", "react", "css", "testing"],
    emergency_contact: {name: "Chris Lee", phone: "+1-555-0195", relationship: "parent"},
    home_address: {street: "55 Mission St", city: "San Francisco", state: "CA", postal_code: "94105", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Sales Rep 1 — reports to VP Sales
EMP_SALES1_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_SALES_ID" --arg mgr "$EMP_VP_SALES_ID" '{fields: {
    full_name: "Kevin Park",
    email: "kevin.park@acme-corp.io",
    phone: "+1-555-0106",
    title: "Senior Account Executive",
    department: $dept,
    manager: $mgr,
    hire_date: "2021-03-15T00:00:00Z",
    salary: 140000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["enterprise-sales", "demo-presentations", "salesforce", "negotiation"],
    emergency_contact: {name: "Mia Park", phone: "+1-555-0196", relationship: "spouse"},
    home_address: {street: "300 Embarcadero", city: "San Francisco", state: "CA", postal_code: "94105", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Sales Rep 2 — reports to VP Sales
EMP_SALES2_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_SALES_ID" --arg mgr "$EMP_VP_SALES_ID" '{fields: {
    full_name: "Rachel Torres",
    email: "rachel.torres@acme-corp.io",
    phone: "+1-555-0107",
    title: "Account Executive",
    department: $dept,
    manager: $mgr,
    hire_date: "2022-07-01T00:00:00Z",
    salary: 110000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["smb-sales", "cold-outreach", "hubspot"],
    emergency_contact: {name: "Elena Torres", phone: "+1-555-0197", relationship: "sister"},
    home_address: {street: "25 Folsom St", city: "San Francisco", state: "CA", postal_code: "94105", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Ops Manager — reports to CEO, in Globex Ops dept
EMP_OPS_MGR_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_OPS_ID" --arg mgr "$EMP_CEO_ID" '{fields: {
    full_name: "Nathan Wright",
    email: "nathan.wright@globex.io",
    phone: "+1-555-0108",
    title: "Operations Manager",
    department: $dept,
    manager: $mgr,
    hire_date: "2020-11-01T00:00:00Z",
    salary: 145000.00,
    employment_type: "full_time",
    status: "active",
    skills: ["operations", "supply-chain", "lean", "six-sigma"],
    certifications: ["Six Sigma Black Belt", "CSCP"],
    emergency_contact: {name: "Amy Wright", phone: "+1-555-0198", relationship: "spouse"},
    home_address: {street: "700 Broadway", city: "Los Angeles", state: "CA", postal_code: "90012", country: "US"},
    owner_id: "system",
    active: true
}}')")

# Intern — on leave
EMP_INTERN_ID=$(create_entity "Employee" "$(jq -n --arg dept "$DEPT_ENG_ID" --arg mgr "$EMP_SR_ENG2_ID" '{fields: {
    full_name: "Casey Nguyen",
    email: "casey.nguyen@acme-corp.io",
    phone: "+1-555-0109",
    title: "Engineering Intern",
    department: $dept,
    manager: $mgr,
    hire_date: "2024-06-01T00:00:00Z",
    salary: 65000.00,
    employment_type: "intern",
    status: "on_leave",
    skills: ["python", "javascript", "sql"],
    emergency_contact: {name: "Linh Nguyen", phone: "+1-555-0189", relationship: "parent"},
    home_address: {street: "1200 College Ave", city: "Berkeley", state: "CA", postal_code: "94702", country: "US"},
    owner_id: "system",
    active: true
}}')")

# =========================================================================
# Layer 5: Companies (no deps — skip contacts[] to avoid circular)
# =========================================================================
echo -e "${CYAN}--- Companies ---${NC}"

COMPANY1_ID=$(create_entity "Company" "$(jq -n '{fields: {
    name: "TechVision Inc.",
    domain: "techvision.io",
    industry: "technology",
    size: "mid_market",
    employee_count: 450,
    annual_revenue: 85000000.00,
    website: "https://techvision.io",
    description: "Enterprise SaaS platform for AI-powered analytics",
    founded_year: 2016,
    headquarters: {street: "500 Howard St", suite: "Suite 300", city: "San Francisco", state: "CA", postal_code: "94105", country: "US", timezone: "America/Los_Angeles"},
    tags: ["saas", "ai", "analytics"],
    metadata: {crm_source: "inbound", lead_quality: "hot"},
    owner_id: "system",
    active: true
}}')")

COMPANY2_ID=$(create_entity "Company" "$(jq -n '{fields: {
    name: "MedCore Health Systems",
    domain: "medcore.health",
    industry: "healthcare",
    size: "enterprise",
    employee_count: 3200,
    annual_revenue: 520000000.00,
    website: "https://medcore.health",
    description: "Integrated healthcare management platform for hospital networks",
    founded_year: 2008,
    headquarters: {street: "1000 Medical Dr", suite: "", city: "Boston", state: "MA", postal_code: "02115", country: "US", timezone: "America/New_York"},
    tags: ["healthcare", "enterprise", "hipaa"],
    metadata: {crm_source: "referral", compliance: ["hipaa", "soc2"]},
    owner_id: "system",
    active: true
}}')")

COMPANY3_ID=$(create_entity "Company" "$(jq -n '{fields: {
    name: "FinLeap Solutions",
    domain: "finleap.co",
    industry: "finance",
    size: "smb",
    employee_count: 85,
    annual_revenue: 12000000.00,
    website: "https://finleap.co",
    description: "Modern payment processing for e-commerce startups",
    founded_year: 2021,
    headquarters: {street: "200 Fintech Blvd", suite: "", city: "Austin", state: "TX", postal_code: "78701", country: "US", timezone: "America/Chicago"},
    tags: ["fintech", "payments", "startup"],
    metadata: {crm_source: "event", event_name: "FinTech Summit 2024"},
    owner_id: "system",
    active: true
}}')")

# =========================================================================
# Layer 6: Contacts (→ Company)
# =========================================================================
echo -e "${CYAN}--- Contacts ---${NC}"

CONTACT1_ID=$(create_entity "Contact" "$(jq -n --arg company "$COMPANY1_ID" '{fields: {
    full_name: "Elena Vasquez",
    email: "elena.vasquez@techvision.io",
    phone: "+1-555-0201",
    title: "CTO",
    company: $company,
    source: "inbound",
    lead_score: 85,
    lifecycle_stage: "opportunity",
    last_contacted: "2025-01-10T14:30:00Z",
    preferred_channel: "email",
    tags: ["decision-maker", "technical"],
    social_profiles: {linkedin: "https://linkedin.com/in/evasquez", twitter: "@evasquez_tech", github: "evasquez"},
    notes: "Key decision maker for the analytics platform deal. Very technical, prefers data-driven demos.",
    owner_id: "system",
    active: true
}}')")

CONTACT2_ID=$(create_entity "Contact" "$(jq -n --arg company "$COMPANY1_ID" '{fields: {
    full_name: "James O\u0027Brien",
    email: "james.obrien@techvision.io",
    phone: "+1-555-0202",
    title: "VP of Engineering",
    company: $company,
    source: "referral",
    lead_score: 72,
    lifecycle_stage: "sql",
    last_contacted: "2025-01-08T10:00:00Z",
    preferred_channel: "linkedin",
    tags: ["influencer", "technical"],
    social_profiles: {linkedin: "https://linkedin.com/in/jobrien", twitter: "", github: ""},
    notes: "Technical evaluator. Runs the platform team at TechVision.",
    owner_id: "system",
    active: true
}}')")

CONTACT3_ID=$(create_entity "Contact" "$(jq -n --arg company "$COMPANY2_ID" '{fields: {
    full_name: "Dr. Amara Okafor",
    email: "a.okafor@medcore.health",
    phone: "+1-555-0203",
    title: "Chief Digital Officer",
    company: $company,
    source: "event",
    lead_score: 90,
    lifecycle_stage: "customer",
    last_contacted: "2025-01-15T09:00:00Z",
    preferred_channel: "phone",
    tags: ["champion", "executive"],
    social_profiles: {linkedin: "https://linkedin.com/in/dramaraokafor", twitter: "", github: ""},
    notes: "Long-standing champion. Drove the initial purchase. Interested in expanding to 3 more hospitals.",
    owner_id: "system",
    active: true
}}')")

CONTACT4_ID=$(create_entity "Contact" "$(jq -n --arg company "$COMPANY3_ID" '{fields: {
    full_name: "Ryan Kim",
    email: "ryan@finleap.co",
    phone: "+1-555-0204",
    title: "Co-founder & CEO",
    company: $company,
    source: "event",
    lead_score: 60,
    lifecycle_stage: "mql",
    last_contacted: "2024-12-20T16:00:00Z",
    preferred_channel: "email",
    tags: ["founder", "budget-holder"],
    social_profiles: {linkedin: "https://linkedin.com/in/ryankimfinleap", twitter: "@ryankim_fin", github: ""},
    notes: "Met at FinTech Summit. Interested but budget-constrained. Follow up in Q2.",
    owner_id: "system",
    active: true
}}')")

CONTACT5_ID=$(create_entity "Contact" "$(jq -n --arg company "$COMPANY2_ID" '{fields: {
    full_name: "Patricia Huang",
    email: "p.huang@medcore.health",
    phone: "+1-555-0205",
    title: "Director of IT",
    company: $company,
    source: "outbound",
    lead_score: 55,
    lifecycle_stage: "lead",
    last_contacted: "2025-01-05T11:30:00Z",
    preferred_channel: "email",
    tags: ["technical", "it-ops"],
    social_profiles: {linkedin: "https://linkedin.com/in/patriciahuang", twitter: "", github: ""},
    notes: "Handles infrastructure decisions. Gate-keeper for technical integrations.",
    owner_id: "system",
    active: true
}}')")

# =========================================================================
# Layer 7: Deals (→ Contact, → Company, → Employee)
# =========================================================================
echo -e "${CYAN}--- Deals ---${NC}"

DEAL1_ID=$(create_entity "Deal" "$(jq -n \
    --arg contact "$CONTACT1_ID" \
    --arg company "$COMPANY1_ID" \
    --arg assigned "$EMP_SALES1_ID" \
    '{fields: {
    name: "TechVision Analytics Platform",
    value: 250000.00,
    currency: "USD",
    stage: "negotiation",
    probability: 75,
    expected_close: "2025-03-15T00:00:00Z",
    contact: $contact,
    company: $company,
    assigned_to: $assigned,
    competitors: ["DataDog", "Splunk"],
    notes: "Multi-year deal for their analytics platform. Legal review in progress.",
    owner_id: "system",
    active: true
}}')")

DEAL2_ID=$(create_entity "Deal" "$(jq -n \
    --arg contact "$CONTACT3_ID" \
    --arg company "$COMPANY2_ID" \
    --arg assigned "$EMP_VP_SALES_ID" \
    '{fields: {
    name: "MedCore Hospital Expansion",
    value: 750000.00,
    currency: "USD",
    stage: "proposal",
    probability: 50,
    expected_close: "2025-06-01T00:00:00Z",
    contact: $contact,
    company: $company,
    assigned_to: $assigned,
    competitors: ["Epic Systems", "Cerner"],
    notes: "Expanding deployment to 3 additional hospitals. Budget approval pending from board.",
    owner_id: "system",
    active: true
}}')")

DEAL3_ID=$(create_entity "Deal" "$(jq -n \
    --arg contact "$CONTACT4_ID" \
    --arg company "$COMPANY3_ID" \
    --arg assigned "$EMP_SALES2_ID" \
    '{fields: {
    name: "FinLeap Starter Package",
    value: 36000.00,
    currency: "USD",
    stage: "qualification",
    probability: 25,
    expected_close: "2025-07-01T00:00:00Z",
    contact: $contact,
    company: $company,
    assigned_to: $assigned,
    notes: "Early stage. Need to demonstrate ROI for their payment processing use case.",
    owner_id: "system",
    active: true
}}')")

DEAL4_ID=$(create_entity "Deal" "$(jq -n \
    --arg contact "$CONTACT3_ID" \
    --arg company "$COMPANY2_ID" \
    --arg assigned "$EMP_VP_SALES_ID" \
    '{fields: {
    name: "MedCore Initial Deployment",
    value: 420000.00,
    currency: "USD",
    stage: "closed_won",
    probability: 100,
    expected_close: "2024-09-01T00:00:00Z",
    actual_close: "2024-08-28T00:00:00Z",
    contact: $contact,
    company: $company,
    assigned_to: $assigned,
    win_reason: "Strong champion, competitive feature set, HIPAA compliance out-of-the-box",
    notes: "Successfully deployed to main hospital campus. Great reference account.",
    owner_id: "system",
    active: true
}}')")

# =========================================================================
# Layer 8: Activities (→ Contact, → Company, → Deal, → Employee)
# =========================================================================
echo -e "${CYAN}--- Activities ---${NC}"

create_entity "Activity" "$(jq -n \
    --arg contact "$CONTACT1_ID" \
    --arg company "$COMPANY1_ID" \
    --arg deal "$DEAL1_ID" \
    --arg performed "$EMP_SALES1_ID" \
    '{fields: {
    subject: "Discovery call with Elena Vasquez",
    activity_type: "call",
    description: "Initial discovery call to understand TechVision analytics requirements. Elena outlined 3 main pain points with current stack.",
    occurred_at: "2024-11-15T14:00:00Z",
    duration_minutes: 45,
    outcome: "completed",
    contact: $contact,
    company: $company,
    deal: $deal,
    performed_by: $performed,
    attendees: ["elena.vasquez@techvision.io", "kevin.park@acme-corp.io"],
    follow_up_date: "2024-11-22T10:00:00Z",
    owner_id: "system"
}}')" > /dev/null

create_entity "Activity" "$(jq -n \
    --arg contact "$CONTACT1_ID" \
    --arg company "$COMPANY1_ID" \
    --arg deal "$DEAL1_ID" \
    --arg performed "$EMP_SALES1_ID" \
    '{fields: {
    subject: "Platform demo for TechVision engineering team",
    activity_type: "demo",
    description: "Full product demo covering real-time analytics, custom dashboards, and API integration. James joined from engineering side.",
    occurred_at: "2024-12-03T10:00:00Z",
    duration_minutes: 90,
    outcome: "completed",
    contact: $contact,
    company: $company,
    deal: $deal,
    performed_by: $performed,
    attendees: ["elena.vasquez@techvision.io", "james.obrien@techvision.io", "kevin.park@acme-corp.io", "marcus.johnson@acme-corp.io"],
    follow_up_date: "2024-12-10T14:00:00Z",
    metadata: {demo_environment: "staging-tv-001", recording_url: "https://internal.acme-corp.io/recordings/tv-demo-001"},
    owner_id: "system"
}}')" > /dev/null

create_entity "Activity" "$(jq -n \
    --arg contact "$CONTACT3_ID" \
    --arg company "$COMPANY2_ID" \
    --arg deal "$DEAL2_ID" \
    --arg performed "$EMP_VP_SALES_ID" \
    '{fields: {
    subject: "Proposal review call with Dr. Okafor",
    activity_type: "call",
    description: "Reviewed expansion proposal. Dr. Okafor is supportive but needs board approval for the budget.",
    occurred_at: "2025-01-15T09:00:00Z",
    duration_minutes: 30,
    outcome: "completed",
    contact: $contact,
    company: $company,
    deal: $deal,
    performed_by: $performed,
    attendees: ["a.okafor@medcore.health", "sarah.mitchell@acme-corp.io"],
    follow_up_date: "2025-02-01T10:00:00Z",
    owner_id: "system"
}}')" > /dev/null

create_entity "Activity" "$(jq -n \
    --arg contact "$CONTACT4_ID" \
    --arg company "$COMPANY3_ID" \
    --arg deal "$DEAL3_ID" \
    --arg performed "$EMP_SALES2_ID" \
    '{fields: {
    subject: "Follow-up email to Ryan Kim",
    activity_type: "email",
    description: "Sent case study and ROI calculator tailored to payment processing companies.",
    occurred_at: "2025-01-08T11:00:00Z",
    duration_minutes: 0,
    outcome: "completed",
    contact: $contact,
    company: $company,
    deal: $deal,
    performed_by: $performed,
    follow_up_date: "2025-01-22T10:00:00Z",
    owner_id: "system"
}}')" > /dev/null

create_entity "Activity" "$(jq -n \
    --arg contact "$CONTACT5_ID" \
    --arg company "$COMPANY2_ID" \
    --arg performed "$EMP_VP_SALES_ID" \
    '{fields: {
    subject: "Technical architecture meeting with MedCore IT",
    activity_type: "meeting",
    description: "Deep-dive on integration architecture for hospital expansion. Discussed HL7 FHIR compliance, SSO requirements, and data residency.",
    occurred_at: "2025-01-12T15:00:00Z",
    duration_minutes: 120,
    outcome: "completed",
    contact: $contact,
    company: $company,
    performed_by: $performed,
    attendees: ["p.huang@medcore.health", "sarah.mitchell@acme-corp.io", "marcus.johnson@acme-corp.io"],
    metadata: {meeting_room: "Zoom", recording: true},
    owner_id: "system"
}}')" > /dev/null

create_entity "Activity" "$(jq -n \
    --arg contact "$CONTACT2_ID" \
    --arg company "$COMPANY1_ID" \
    --arg deal "$DEAL1_ID" \
    --arg performed "$EMP_SALES1_ID" \
    '{fields: {
    subject: "Contract negotiation with TechVision legal",
    activity_type: "meeting",
    description: "Reviewed MSA and SOW with their legal counsel. Minor redlines on liability cap and SLA terms.",
    occurred_at: "2025-01-20T13:00:00Z",
    duration_minutes: 60,
    outcome: "rescheduled",
    contact: $contact,
    company: $company,
    deal: $deal,
    performed_by: $performed,
    attendees: ["james.obrien@techvision.io", "kevin.park@acme-corp.io"],
    follow_up_date: "2025-01-27T13:00:00Z",
    owner_id: "system"
}}')" > /dev/null

# =========================================================================
# Layer 9: Projects (→ Department, → Employee, → Employee[], → Company)
# =========================================================================
echo -e "${CYAN}--- Projects ---${NC}"

PROJECT1_ID=$(create_entity "Project" "$(jq -n \
    --arg dept "$DEPT_ENG_ID" \
    --arg lead "$EMP_VP_ENG_ID" \
    --arg member1 "$EMP_SR_ENG1_ID" \
    --arg member2 "$EMP_SR_ENG2_ID" \
    --arg member3 "$EMP_JR_ENG_ID" \
    --arg client "$COMPANY1_ID" \
    '{fields: {
    name: "Platform v2.0",
    code: "PLAT-V2",
    description: "Major platform rewrite: new API layer, real-time analytics engine, and redesigned admin dashboard. Targeting 10x throughput improvement.",
    status: "active",
    priority: "high",
    start_date: "2025-01-06T00:00:00Z",
    target_end_date: "2025-06-30T00:00:00Z",
    budget: 500000.00,
    spent: 75000.00,
    department: $dept,
    lead: $lead,
    members: [$member1, $member2, $member3],
    client: $client,
    tags: ["platform", "v2", "rewrite"],
    metadata: {sprint_length_days: 14, current_sprint: 3},
    owner_id: "system",
    active: true
}}')")

PROJECT2_ID=$(create_entity "Project" "$(jq -n \
    --arg dept "$DEPT_SALES_ID" \
    --arg lead "$EMP_VP_SALES_ID" \
    --arg member1 "$EMP_SALES1_ID" \
    --arg member2 "$EMP_SALES2_ID" \
    '{fields: {
    name: "Sales Dashboard",
    code: "SALES-DASH",
    description: "Interactive sales analytics dashboard with pipeline forecasting, rep performance tracking, and territory management.",
    status: "planning",
    priority: "medium",
    start_date: "2025-02-15T00:00:00Z",
    target_end_date: "2025-04-30T00:00:00Z",
    budget: 120000.00,
    spent: 0.00,
    department: $dept,
    lead: $lead,
    members: [$member1, $member2],
    tags: ["sales", "dashboard", "analytics"],
    owner_id: "system",
    active: true
}}')")

# =========================================================================
# Layer 10: Tasks (→ Project, → Employee, → Task[] for blocked_by)
# =========================================================================
echo -e "${CYAN}--- Tasks ---${NC}"

TASK1_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg assignee "$EMP_SR_ENG1_ID" \
    --arg reviewer "$EMP_VP_ENG_ID" \
    '{fields: {
    title: "Design new API gateway architecture",
    description: "Document the target architecture for the v2 API gateway. Include rate limiting, auth, versioning, and caching layers.",
    status: "done",
    priority: "critical",
    story_points: 8,
    completed_at: "2025-01-17T16:00:00Z",
    estimated_hours: 16.0,
    actual_hours: 14.5,
    project: $project,
    assignee: $assignee,
    reviewer: $reviewer,
    tags: ["architecture", "api"],
    labels: ["documentation"],
    owner_id: "system"
}}')")

TASK2_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg assignee "$EMP_SR_ENG2_ID" \
    --arg reviewer "$EMP_SR_ENG1_ID" \
    --arg blocked "$TASK1_ID" \
    '{fields: {
    title: "Implement API gateway middleware",
    description: "Build the core API gateway middleware based on the approved architecture document. Implement auth, rate limiting, and request routing.",
    status: "in_progress",
    priority: "high",
    story_points: 13,
    estimated_hours: 32.0,
    actual_hours: 18.0,
    project: $project,
    assignee: $assignee,
    reviewer: $reviewer,
    blocked_by: [$blocked],
    tags: ["api", "middleware"],
    labels: ["feature"],
    owner_id: "system"
}}')")

TASK3_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg assignee "$EMP_JR_ENG_ID" \
    --arg reviewer "$EMP_SR_ENG1_ID" \
    '{fields: {
    title: "Build real-time analytics React components",
    description: "Create reusable React components for real-time data visualization: line charts, bar charts, sparklines, and KPI cards.",
    status: "in_review",
    priority: "high",
    story_points: 8,
    estimated_hours: 20.0,
    actual_hours: 22.0,
    project: $project,
    assignee: $assignee,
    reviewer: $reviewer,
    tags: ["frontend", "react", "charts"],
    labels: ["feature"],
    owner_id: "system"
}}')")

TASK4_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg assignee "$EMP_SR_ENG1_ID" \
    '{fields: {
    title: "Set up CI/CD pipeline for v2",
    description: "Configure GitHub Actions for the v2 platform: build, test, lint, Docker image push, and staging deployment.",
    status: "todo",
    priority: "medium",
    story_points: 5,
    estimated_hours: 10.0,
    project: $project,
    assignee: $assignee,
    tags: ["devops", "ci-cd"],
    labels: ["infrastructure"],
    owner_id: "system"
}}')")

TASK5_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg blocked1 "$TASK2_ID" \
    --arg blocked2 "$TASK3_ID" \
    '{fields: {
    title: "Integration testing for analytics pipeline",
    description: "Write end-to-end integration tests for the full analytics pipeline: data ingestion, processing, and real-time dashboard rendering.",
    status: "backlog",
    priority: "medium",
    story_points: 8,
    estimated_hours: 20.0,
    project: $project,
    blocked_by: [$blocked1, $blocked2],
    tags: ["testing", "integration"],
    labels: ["feature"],
    owner_id: "system"
}}')")

TASK6_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg assignee "$EMP_SR_ENG2_ID" \
    '{fields: {
    title: "Fix memory leak in WebSocket handler",
    description: "Investigate and fix the memory leak reported in the WebSocket connection handler. Connections are not being properly cleaned up on client disconnect.",
    status: "in_progress",
    priority: "critical",
    story_points: 3,
    estimated_hours: 6.0,
    actual_hours: 2.0,
    project: $project,
    assignee: $assignee,
    tags: ["bug", "websocket"],
    labels: ["bug"],
    owner_id: "system"
}}')")

TASK7_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT2_ID" \
    --arg assignee "$EMP_SALES1_ID" \
    '{fields: {
    title: "Define dashboard KPI requirements",
    description: "Work with sales leadership to define the key metrics, filters, and drill-down capabilities needed for the sales dashboard.",
    status: "todo",
    priority: "high",
    story_points: 5,
    estimated_hours: 12.0,
    project: $project,
    assignee: $assignee,
    tags: ["requirements", "kpi"],
    labels: ["documentation"],
    owner_id: "system"
}}')")

TASK8_ID=$(create_entity "Task" "$(jq -n \
    --arg project "$PROJECT2_ID" \
    '{fields: {
    title: "Evaluate BI tool integrations",
    description: "Research and evaluate integration options: embedded Metabase, custom Grafana dashboards, or building from scratch with D3.js.",
    status: "backlog",
    priority: "low",
    story_points: 3,
    estimated_hours: 8.0,
    project: $project,
    tags: ["research", "tools"],
    labels: ["improvement"],
    owner_id: "system"
}}')")

# =========================================================================
# Layer 11: Milestones (→ Project, → Task[])
# =========================================================================
echo -e "${CYAN}--- Milestones ---${NC}"

create_entity "Milestone" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg task1 "$TASK1_ID" \
    --arg task4 "$TASK4_ID" \
    '{fields: {
    name: "Alpha Release",
    description: "Core API gateway and basic analytics pipeline operational in staging environment.",
    due_date: "2025-02-28T00:00:00Z",
    completed_at: "2025-02-25T17:00:00Z",
    status: "completed",
    project: $project,
    tasks: [$task1, $task4],
    owner_id: "system"
}}')" > /dev/null

create_entity "Milestone" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg task2 "$TASK2_ID" \
    --arg task3 "$TASK3_ID" \
    --arg task6 "$TASK6_ID" \
    '{fields: {
    name: "Beta Release",
    description: "Full analytics pipeline with real-time dashboard, WebSocket support, and performance benchmarks.",
    due_date: "2025-04-30T00:00:00Z",
    status: "in_progress",
    project: $project,
    tasks: [$task2, $task3, $task6],
    owner_id: "system"
}}')" > /dev/null

create_entity "Milestone" "$(jq -n \
    --arg project "$PROJECT2_ID" \
    --arg task7 "$TASK7_ID" \
    '{fields: {
    name: "Sales Dashboard MVP",
    description: "Minimum viable dashboard with pipeline overview, rep leaderboard, and monthly trends.",
    due_date: "2025-03-31T00:00:00Z",
    status: "upcoming",
    project: $project,
    tasks: [$task7],
    owner_id: "system"
}}')" > /dev/null

# =========================================================================
# Layer 12: Documents (→ Project, → Deal, → Employee, → Employee[])
# =========================================================================
echo -e "${CYAN}--- Documents ---${NC}"

DOC1_ID=$(create_entity "Document" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg author "$EMP_VP_ENG_ID" \
    --arg reviewer1 "$EMP_SR_ENG1_ID" \
    --arg reviewer2 "$EMP_SR_ENG2_ID" \
    '{fields: {
    title: "Platform v2.0 Architecture Specification",
    content: "# Platform v2.0 Architecture\n\n## Overview\nThis document describes the target architecture for Platform v2.0, covering the API gateway, analytics engine, real-time pipeline, and admin dashboard.\n\n## API Gateway\n- Rate limiting: token bucket algorithm\n- Auth: JWT with refresh tokens\n- Versioning: URL-based (/v1, /v2)\n\n## Analytics Engine\n- Stream processing with Apache Kafka\n- Materialized views for dashboard queries\n- Sub-second query latency target",
    summary: "Complete architecture specification for the Platform v2.0 rewrite",
    doc_type: "specification",
    status: "approved",
    version_number: 3,
    confidential: false,
    classification: "internal",
    project: $project,
    author: $author,
    reviewers: [$reviewer1, $reviewer2],
    tags: ["architecture", "v2", "specification"],
    owner_id: "system"
}}')")

DOC2_ID=$(create_entity "Document" "$(jq -n \
    --arg deal "$DEAL2_ID" \
    --arg author "$EMP_VP_SALES_ID" \
    --arg reviewer "$EMP_CEO_ID" \
    '{fields: {
    title: "MedCore Hospital Expansion Proposal",
    content: "# MedCore Health Systems — Expansion Proposal\n\n## Executive Summary\nProposal to expand the SchemaForge deployment from MedCore main campus to 3 additional hospital facilities.\n\n## Scope\n- 3 hospitals: Boston General, Cambridge Medical, Quincy Health\n- 3,200 additional users\n- Custom HL7 FHIR integration\n- 24/7 support SLA\n\n## Pricing\n- Annual license: $750,000\n- Implementation: $150,000\n- Total Year 1: $900,000",
    summary: "Expansion proposal for MedCore deployment to 3 additional hospitals",
    doc_type: "proposal",
    status: "review",
    version_number: 2,
    confidential: true,
    classification: "confidential",
    internal_notes: "Board meeting scheduled for Feb 15. Dr. Okafor confirmed she will champion internally. CFO may push back on Y1 implementation cost.",
    related_deal: $deal,
    author: $author,
    reviewers: [$reviewer],
    tags: ["proposal", "medcore", "expansion"],
    owner_id: "system"
}}')")

DOC3_ID=$(create_entity "Document" "$(jq -n \
    --arg project "$PROJECT1_ID" \
    --arg author "$EMP_SR_ENG1_ID" \
    '{fields: {
    title: "Sprint 3 Retrospective",
    content: "# Sprint 3 Retrospective\n\n## What went well\n- API gateway middleware ahead of schedule\n- Good collaboration between frontend and backend\n- Zero critical bugs in staging\n\n## What could improve\n- WebSocket memory leak blocked testing\n- Need better staging data for analytics pipeline\n- Documentation lagging behind implementation\n\n## Action items\n- [ ] Fix WebSocket memory leak (Priya, P0)\n- [ ] Set up seed data script for staging\n- [ ] Schedule doc sprint for next week",
    summary: "Retrospective notes from Platform v2.0 Sprint 3",
    doc_type: "meeting_notes",
    status: "approved",
    version_number: 1,
    confidential: false,
    classification: "internal",
    project: $project,
    author: $author,
    tags: ["retrospective", "sprint-3"],
    owner_id: "system"
}}')")

# =========================================================================
# Layer 13: Comments (→ Employee, → Comment for threading)
# =========================================================================
echo -e "${CYAN}--- Comments ---${NC}"

COMMENT1_ID=$(create_entity "Comment" "$(jq -n \
    --arg author "$EMP_SR_ENG1_ID" \
    --arg task "$TASK2_ID" \
    '{fields: {
    preview: "Should we use tower middleware or custom?",
    body: "For the API gateway middleware, should we build on tower::Service or write a custom middleware layer? Tower gives us composability but the learning curve is steep for the team. Thoughts?",
    parent_type: "Task",
    parent_id: $task,
    author: $author,
    reactions: {"thumbsup": 2, "thinking": 1},
    edited: false,
    owner_id: "system"
}}')")

COMMENT2_ID=$(create_entity "Comment" "$(jq -n \
    --arg author "$EMP_SR_ENG2_ID" \
    --arg task "$TASK2_ID" \
    --arg parent "$COMMENT1_ID" \
    '{fields: {
    preview: "Tower is the way to go",
    body: "I vote tower. The composability is worth the learning curve — we can reuse the rate limiter and auth layers across services. I can run a brown bag session for the team.",
    parent_type: "Task",
    parent_id: $task,
    parent_comment: $parent,
    author: $author,
    reactions: {"thumbsup": 3},
    edited: false,
    owner_id: "system"
}}')")

create_entity "Comment" "$(jq -n \
    --arg author "$EMP_VP_ENG_ID" \
    --arg doc "$DOC1_ID" \
    '{fields: {
    preview: "Architecture doc approved with minor feedback",
    body: "Great work on the architecture spec. Two minor suggestions:\n1. Add a section on observability (metrics, tracing, logging)\n2. Include a capacity planning estimate for the analytics pipeline\n\nOtherwise, approved for implementation.",
    parent_type: "Document",
    parent_id: $doc,
    author: $author,
    reactions: {"rocket": 1},
    edited: false,
    owner_id: "system"
}}')" > /dev/null

create_entity "Comment" "$(jq -n \
    --arg author "$EMP_JR_ENG_ID" \
    --arg project "$PROJECT1_ID" \
    '{fields: {
    preview: "React component library question",
    body: "Quick question about the React analytics components — should I use Recharts or Nivo for the charting library? Recharts has better TypeScript support but Nivo has more chart types out of the box.",
    parent_type: "Project",
    parent_id: $project,
    author: $author,
    edited: false,
    owner_id: "system"
}}')" > /dev/null

# =========================================================================
# Layer 14: Workflows (no dependencies — text fields only)
# =========================================================================
echo -e "${CYAN}--- Workflows ---${NC}"

create_entity "Workflow" "$(jq -n '{fields: {
    name: "Deal Stage Notification",
    description: "Send notifications when deals move to negotiation or closed_won stages",
    target_schema: "Deal",
    trigger_field: "stage",
    rules: {
        conditions: [
            {field: "stage", operator: "in", values: ["negotiation", "closed_won"]},
            {field: "value", operator: "gte", value: 100000}
        ],
        actions: [
            {type: "notify", channel: "slack", target: "#sales-wins", template: "Deal {{name}} moved to {{stage}} (${{value}})"},
            {type: "notify", channel: "email", target: "sales-leadership@acme-corp.io", template: "High-value deal update"}
        ]
    },
    enabled: true,
    execution_count: 12,
    last_executed: "2025-01-20T14:30:00Z"
}}')" > /dev/null

# =========================================================================
# Summary
# =========================================================================
echo ""
echo -e "${BOLD}========================================${NC}"
if [ "$ERRORS" -eq 0 ]; then
    echo -e "${GREEN}${BOLD}Seeding complete!${NC} ${TOTAL} entities created successfully."
else
    echo -e "${YELLOW}${BOLD}Seeding finished with errors.${NC} ${TOTAL} attempted, ${RED}${ERRORS} failed${NC}."
fi
echo -e "${BOLD}========================================${NC}"
echo ""
echo -e "  Server:    ${BASE_URL}"
echo -e "  Admin UI:  ${BASE_URL}/admin/"
echo -e "  Schemas:   ${API}"
echo ""
