import { test, expect, type Page } from "@playwright/test"

// Minimal end-to-end smoke for the generated React site. Intentionally
// narrow — covers login, one admin CRUD round-trip (including a composite
// sub-field), and a users listing assertion. Grow this as the surface
// changes; do not turn it into a component-level test.

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
  // The default landing is the admin schema catalog ("/admin"). Match
  // either /admin or any /app/<entity> page so this test stays decoupled
  // from changes to App.tsx's root Navigate target.
  await expect(page).toHaveURL(/\/(admin|app\/)/)
})

test("admin create → detail → delete round-trip on Company", async ({ page }) => {
  await login(page)

  // List → New
  await page.goto("/admin/Company")
  await page.getByRole("button", { name: /new company/i }).click()
  await expect(page).toHaveURL(/\/admin\/Company\/new/)

  // Fill top-level fields plus a composite sub-field. Visible labels show
  // a required-asterisk after the field name; we target the inputs by
  // their accessible name (aria-hidden hides the asterisk from AT) via
  // getByRole.
  await page.getByRole("textbox", { name: "name" }).fill("Playwright Test Co")
  await page.getByRole("textbox", { name: "city" }).fill("Austin")

  await page.getByRole("button", { name: /^create$/i }).click()

  // Detail view should render the values we just submitted. The headline
  // and the spec-row both show the name; scope to the heading so strict
  // mode passes.
  await expect(page).toHaveURL(/\/admin\/Company\/[^/]+$/)
  await expect(
    page.getByRole("heading", { name: "Playwright Test Co" }),
  ).toBeVisible()
  await expect(page.getByText("Austin")).toBeVisible()

  // Back to the list and delete. Destructive actions now route through the
  // accessible Radix AlertDialog (Section 508 audit F-001) instead of
  // window.confirm — hover the row so the row-actions reveal, then click
  // Delete to open the dialog and confirm inside the alertdialog scope.
  await page.goto("/admin/Company")
  const row = page.getByRole("row", { name: /Playwright Test Co/i })
  await row.hover()
  await row.getByRole("button", { name: /^delete$/i }).click()
  const dialog = page.getByRole("alertdialog")
  await expect(dialog).toBeVisible()
  await dialog.getByRole("button", { name: /^delete$/i }).click()
  await expect(page.getByText("Playwright Test Co")).toHaveCount(0)
})

test("/admin/users lists the bootstrapped admin", async ({ page }) => {
  await login(page)
  await page.goto("/admin/users")
  // Strict mode: there can be multiple cells containing "admin" (the role
  // chip says "platform_admin", display name says "Administrator"). Bind
  // the assertion to the id-cell column where the row identifier lives.
  await expect(
    page.getByRole("cell", { name: USERNAME, exact: true }),
  ).toBeVisible()
})
