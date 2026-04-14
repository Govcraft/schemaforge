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
  // The default landing is the first codegen'd /app/<entity> page.
  await expect(page).toHaveURL(/\/app\//)
})

test("admin create → detail → delete round-trip on Company", async ({ page }) => {
  await login(page)

  // List → New
  await page.goto("/admin/Company")
  await page.getByRole("button", { name: /new company/i }).click()
  await expect(page).toHaveURL(/\/admin\/Company\/new/)

  // Fill top-level fields plus a composite sub-field. The city input is
  // keyed by the dot-path `address.city` in react-hook-form, but the
  // rendered label is just "city" — grab it by name attribute.
  const nameInput = page.getByLabel("name")
  await nameInput.fill("Playwright Test Co")
  await page.locator('input[name="address.city"]').fill("Austin")

  await page.getByRole("button", { name: /^create$/i }).click()

  // Detail view should render the values we just submitted.
  await expect(page).toHaveURL(/\/admin\/Company\/[^/]+$/)
  await expect(page.getByText("Playwright Test Co")).toBeVisible()
  await expect(page.getByText("Austin")).toBeVisible()

  // Back to the list and delete.
  page.on("dialog", (d) => d.accept())
  await page.goto("/admin/Company")
  await page
    .getByRole("row", { name: /Playwright Test Co/i })
    .getByRole("button", { name: /delete/i })
    .click()
  await expect(page.getByText("Playwright Test Co")).toHaveCount(0)
})

test("/admin/users lists the bootstrapped admin", async ({ page }) => {
  await login(page)
  await page.goto("/admin/users")
  await expect(page.getByRole("cell", { name: USERNAME })).toBeVisible()
})
