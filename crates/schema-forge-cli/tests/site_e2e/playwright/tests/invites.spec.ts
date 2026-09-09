import { test, expect } from "@playwright/test"

test("public invitation validates confirmation and submits without credentials", async ({ page }) => {
  let body: unknown
  await page.route("**/api/v1/forge/auth/invites/accept", async (route) => {
    expect(route.request().headers().authorization).toBeUndefined()
    body = route.request().postDataJSON()
    await route.fulfill({ status: 201, json: { email: "invitee@example.com", roles: ["member"] } })
  })
  await page.goto("/invite/accept?invite=opaque-reference")
  await page.getByLabel("Password (at least 8 characters)").fill("example-password")
  await page.getByLabel("Confirm password").fill("different-password")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("alert")).toHaveText("Passwords must match.")
  expect(body).toBeUndefined()
  await page.getByLabel("Confirm password").fill("example-password")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page).toHaveURL(/\/login\?invitation=accepted$/)
  expect(body).toEqual({ invite_id: "opaque-reference", password: "example-password" })
})

test("expired invitation stays on acceptance page with an actionable error", async ({ page }) => {
  await page.route("**/api/v1/forge/auth/invites/accept", (route) => route.fulfill({ status: 401, json: { error: "invalid_invitation" } }))
  await page.goto("/invite/accept?invite=expired")
  await page.getByLabel("Password (at least 8 characters)").fill("example-password")
  await page.getByLabel("Confirm password").fill("example-password")
  await page.getByRole("button", { name: "Create account" }).click()
  await expect(page.getByRole("alert")).toContainText("Ask your administrator for a new invitation")
  await expect(page).toHaveURL(/\/invite\/accept/)
})

test("missing invitation reference does not expose a submission form", async ({ page }) => {
  await page.goto("/invite/accept")
  await expect(page.getByRole("alert")).toContainText("missing its reference")
  await expect(page.getByRole("button", { name: "Create account" })).toHaveCount(0)
})

test("operator can invite with only writable tenant choices", async ({ page }) => {
  await page.addInitScript(() => {
    sessionStorage.setItem("schemaforge.token", "test-session")
    sessionStorage.setItem("schemaforge.token_expires_at", new Date(Date.now() + 3600000).toISOString())
  })
  await page.route("**/api/v1/forge/schemas", (route) => route.fulfill({ json: { schemas: [
    { name: "User", annotations: [{ annotation: "System" }], permissions: { create: true } },
    { name: "Organization", annotations: [{ annotation: "Tenant", Root: null }] },
  ] } }))
  await page.route("**/api/v1/forge/users/roles", (route) => route.fulfill({ json: { roles: [{ name: "member", rank: 1 }] } }))
  await page.route("**/api/v1/forge/auth/me", (route) => route.fulfill({ json: { username: "admin", roles: [], memberships: [] } }))
  await page.route("**/api/v1/forge/schemas/Organization/entities?*", (route) => route.fulfill({ json: { total_count: 102, entities: new URL(route.request().url()).searchParams.get("offset") === "0" ? [] : [
    { id: "writable", fields: { name: "Writable organization" }, permissions: { update: true } },
    { id: "readonly", fields: { name: "Read-only organization" }, permissions: { update: false } },
  ] } }))
  let body: unknown
  await page.route("**/api/v1/forge/auth/invites", async (route) => {
    body = route.request().postDataJSON()
    await route.fulfill({ status: 201, json: { invite_id: "issued", email: "invitee@example.com", expires_at: "2030-01-01T00:00:00Z" } })
  })
  await page.goto("/admin/users/invite")
  await page.getByLabel("Email", { exact: true }).fill("invitee@example.com")
  await page.getByLabel("Role (optional)").selectOption("member")
  await page.getByLabel("Tenant type (optional)").selectOption("Organization")
  await expect(page.getByRole("option", { name: "Read-only organization" })).toHaveCount(0)
  await page.getByLabel("Tenant", { exact: true }).selectOption("writable")
  await page.getByRole("button", { name: "Send invitation" }).click()
  await expect(page.getByRole("status")).toContainText("Invitation sent to invitee@example.com")
  expect(body).toEqual({ email: "invitee@example.com", role: "member", tenant_type: "Organization", tenant_id: "writable" })
})
