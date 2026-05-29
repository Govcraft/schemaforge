import { test, expect, type Page } from "@playwright/test"

// Minimal end-to-end smoke for the generated React site. Intentionally
// narrow — covers login and one create → detail round-trip on the `/app`
// per-entity surface. Grow this as the surface changes; do not turn it into
// a component-level test.
//
// NOTE: the runtime-dynamic admin console (the old `/admin/*` shell, schema
// catalog, generic CRUD, and user management) moved to the schemaforge-console
// repo. There is no delete or user-management UI in the generated `/app`
// scaffold, so those round-trips are no longer covered here.

const USERNAME = process.env.FORGE_ADMIN_USER ?? "admin"
const PASSWORD = process.env.FORGE_ADMIN_PASSWORD ?? "admin"

async function login(page: Page) {
  await page.goto("/login")
  await page.getByLabel("Username").fill(USERNAME)
  await page.getByLabel("Password").fill(PASSWORD)
  await page.getByRole("button", { name: /sign in/i }).click()
  await expect(page).not.toHaveURL(/\/login/)
}

test("login lands the user away from /login", async ({ page }) => {
  await login(page)
  // The default landing redirects to the first declared entity's `/app` list.
  await expect(page).toHaveURL(/\/app\//)
})

test("create → detail round-trip on Company", async ({ page }) => {
  await login(page)

  // List → New. The "New" control is a styled react-router <Link> (role
  // link), not a <button>.
  await page.goto("/app/company")
  await page.getByRole("link", { name: /new company/i }).click()
  await expect(page).toHaveURL(/\/app\/company\/new/)

  // Only `name` is required on the demo Company schema. Target by label so
  // the test is robust to label casing.
  await page.getByLabel(/^name/i).fill("Playwright Test Co")
  await page.getByRole("button", { name: /^create$/i }).click()

  // A successful create redirects to the detail view at /app/company/:id and
  // renders the name as the page headline.
  await expect(page).toHaveURL(/\/app\/company\/[^/]+$/)
  await expect(
    page.getByRole("heading", { name: "Playwright Test Co" }),
  ).toBeVisible()
})
