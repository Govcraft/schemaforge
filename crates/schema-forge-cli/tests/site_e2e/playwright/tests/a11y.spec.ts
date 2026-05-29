import { test, expect, type Page } from "@playwright/test"
import AxeBuilder from "@axe-core/playwright"

// Automated WCAG 2.1 A + AA scan of the generated site's primary routes.
// Required by federal procurement under FAR 39.2 and the project's internal
// remediation goal from the 2026-05-17 Section 508 audit. Each test asserts
// zero axe violations at the WCAG 2.1 A / AA / Section 508 tag levels.
//
// Keep the scan list narrow: cover the routes a federal-trusted-tester walks
// (login, an entity list, an entity create form, and an entity detail) so any
// future regression in template-level a11y trips CI on the next site generate.
// The old `/admin/*` shell moved to the schemaforge-console repo and is scanned
// there.

const USERNAME = process.env.FORGE_ADMIN_USER ?? "admin"
const PASSWORD = process.env.FORGE_ADMIN_PASSWORD ?? "admin"
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "section508"]

async function login(page: Page) {
  await page.goto("/login")
  await page.getByLabel("Username").fill(USERNAME)
  await page.getByLabel("Password").fill(PASSWORD)
  await page.getByRole("button", { name: /sign in/i }).click()
  await expect(page).not.toHaveURL(/\/login/)
}

async function scan(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze()
  return results.violations
}

test("login page has zero axe violations", async ({ page }) => {
  await page.goto("/login")
  await expect(page.getByRole("heading", { name: /sign in to/i })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("Company list has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/app/company")
  await expect(page.getByRole("heading", { name: "Company" })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("Company create form has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/app/company/new")
  await expect(page.getByRole("heading", { name: /new company/i })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("Company detail has zero axe violations", async ({ page }) => {
  await login(page)

  // Seed a record so there is a detail page to scan. A successful create
  // redirects to /app/company/:id.
  await page.goto("/app/company/new")
  await page.getByLabel(/^name/i).fill("Axe Subject Co")
  await page.getByRole("button", { name: /^create$/i }).click()
  await expect(page).toHaveURL(/\/app\/company\/[^/]+$/)
  await expect(
    page.getByRole("heading", { name: "Axe Subject Co" }),
  ).toBeVisible()

  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})
