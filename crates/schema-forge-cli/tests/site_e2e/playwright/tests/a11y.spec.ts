import { test, expect, type Page } from "@playwright/test"
import AxeBuilder from "@axe-core/playwright"

// Automated WCAG 2.1 A + AA scan of the generated site's primary routes.
// Required by federal procurement under FAR 39.2 and the project's internal
// remediation goal from the 2026-05-17 Section 508 audit. Each test asserts
// zero axe violations at the WCAG 2.1 A / AA / Section 508 tag levels.
//
// Keep the scan list narrow: cover the routes a federal-trusted-tester walks
// (login, the admin dashboard, an entity list/edit, the users list) so any
// future regression in template-level a11y trips CI on the next site
// generate.

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

test("admin schemas index has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/admin")
  await expect(page.getByRole("heading", { name: /schema catalog/i })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("admin Company list has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/admin/Company")
  await expect(page.getByRole("heading", { name: "Company" })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("admin Company create form has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/admin/Company/new")
  await expect(page.getByRole("heading", { name: /new company/i })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("admin users list has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/admin/users")
  await expect(page.getByRole("heading", { name: /users/i })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("admin users new form has zero axe violations", async ({ page }) => {
  await login(page)
  await page.goto("/admin/users/new")
  await expect(page.getByRole("heading", { name: /new user/i })).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])
})

test("delete confirmation dialog has zero axe violations", async ({ page }) => {
  await login(page)

  // Seed a record so there is a row to delete.
  await page.goto("/admin/Company/new")
  await page.getByRole("textbox", { name: "name" }).fill("Axe Subject Co")
  await page.getByRole("textbox", { name: "city" }).fill("Boston")
  await page.getByRole("button", { name: /^create$/i }).click()
  await expect(page).toHaveURL(/\/admin\/Company\/[^/]+$/)

  // Open the AlertDialog from the detail page and scan it in place.
  await page.getByRole("button", { name: /^delete$/i }).click()
  const dialog = page.getByRole("alertdialog")
  await expect(dialog).toBeVisible()
  const violations = await scan(page)
  expect(violations, JSON.stringify(violations, null, 2)).toEqual([])

  // Tidy up so the record does not leak into subsequent runs.
  await dialog.getByRole("button", { name: /^delete$/i }).click()
  await expect(page).toHaveURL(/\/admin\/Company$/)
})
