import { test, expect, type Page } from "@playwright/test"

async function login(page: Page) {
  await page.goto("/login")
  await page.getByLabel("Username").fill(process.env.FORGE_ADMIN_USER ?? "admin")
  await page.getByLabel("Password").fill(process.env.FORGE_ADMIN_PASSWORD ?? "admin")
  await page.getByRole("button", { name: /sign in/i }).click()
  await expect(page).not.toHaveURL(/\/login/)
}

// Exercise the client role hints independently of server authorization: retain
// the real admin token while changing only the cached roles consumed by forms.
async function roleHint(page: Page, roles: string[]) {
  await page.evaluate(roles => sessionStorage.setItem("schemaforge.roles", JSON.stringify(roles)), roles)
}

test("duration, map, base64 and nested fields survive create and edit", async ({ page }) => {
  await login(page)
  await page.goto("/app/job/new")
  await expect(page.locator('input[name="computed_label"]')).toHaveCount(0)
  // platform_admin follows the global Cedar permit, including restricted fields.
  await expect(page.getByLabel(/^finance only/i)).toBeVisible()
  await page.getByLabel(/^name/i).fill("Extended field job")
  await page.getByLabel(/^timeout/i).fill("1h30m")
  await page.getByLabel(/^labels/i).fill('{"attempts": 3}')
  await page.getByLabel(/^checksum/i).fill("aGVsbG8=")
  await page.getByLabel(/^delays/i).fill('["1s", "2m"]')
  await page.getByLabel(/^delay$/i).fill("2m")
  await page.getByLabel(/^counters/i).fill('{"success": 2}')
  await page.getByLabel(/^review note/i).fill("reviewed")
  await page.getByLabel(/^public note/i).fill("public sibling")
  await page.getByLabel(/^private note/i).fill("protected sibling")
  const created = page.waitForResponse(response => response.request().method() === "POST" && response.url().endsWith("/schemas/Job/entities"))
  await page.getByRole("button", { name: /^create$/i }).click()
  const response = await created
  expect(response.status(), await response.text()).toBe(201)
  const payload = response.request().postDataJSON().fields
  expect(payload).toMatchObject({ timeout: "1h30m", labels: { attempts: 3 }, checksum: "aGVsbG8=", delays: ["1s", "2m"], settings: { delay: "2m", counters: { success: 2 } } })
  expect(payload).not.toHaveProperty("computed_label")
  await expect(page).toHaveURL(/\/app\/job\/job_[a-z0-9]+$/)
  await expect(page.getByText("1h 30m", { exact: true })).toBeVisible()
  await expect(page.getByText("Extended field job-computed", { exact: true })).toBeVisible()
  const detailUrl = page.url()
  await page.goto(`${detailUrl}/edit`)
  await expect(page.getByLabel(/^timeout/i)).toHaveValue("5400s")
  await expect(page.getByLabel(/^checksum/i)).toHaveValue("aGVsbG8=")
  await expect(page.getByLabel(/^labels/i)).toHaveValue(/"attempts": 3/)
  await expect(page.getByLabel(/^counters/i)).toHaveValue(/"success": 2/)
  const saved = page.waitForRequest(request => request.method() === "PATCH" && request.url().includes("/schemas/Job/entities/"))
  await page.getByRole("button", { name: /^save changes$/i }).click()
  expect((await saved).postDataJSON().fields).not.toHaveProperty("computed_label")
  await expect(page).toHaveURL(detailUrl)
  await roleHint(page, ["clerk"])
  await page.goto(`${detailUrl}/edit`)
  await expect(page.getByRole("note", { name: "Protected Group (read only)" })).toContainText("protected sibling")
  await expect(page.locator('input[name="protected_group.public_note"]')).toHaveCount(0)
  await page.getByLabel(/^name/i).fill("Clerk preserves siblings")
  const restrictedSave = page.waitForRequest(request => request.method() === "PATCH" && request.url().includes("/schemas/Job/entities/"))
  await page.getByRole("button", { name: /^save changes$/i }).click()
  expect((await restrictedSave).postDataJSON().fields).not.toHaveProperty("protected_group")
  await expect(page).toHaveURL(detailUrl)
  await expect(page.getByText("protected sibling", { exact: true })).toBeVisible()
})

test("role hints hide unreadable controls and omit unwritable required values", async ({ page }) => {
  await login(page)
  await roleHint(page, ["clerk"])
  await page.goto("/app/job/new")
  await expect(page.locator('input[name="finance_only"]')).toHaveCount(0)
  await expect(page.getByRole("note", { name: "Review Note (read only)" })).toBeVisible()
  await expect(page.locator('input[name="review_note"]')).toHaveCount(0)
  await page.getByLabel(/^name/i).fill("Clerk field job")
  await page.getByLabel(/^timeout/i).fill("90s")
  const created = page.waitForResponse(response => response.request().method() === "POST" && response.url().endsWith("/schemas/Job/entities"))
  await page.getByRole("button", { name: /^create$/i }).click()
  const response = await created
  expect(response.status(), await response.text()).toBe(201)
  const payload = response.request().postDataJSON().fields
  expect(payload).not.toHaveProperty("review_note")
  expect(payload).not.toHaveProperty("finance_only")
  expect(payload).not.toHaveProperty("computed_label")
  await expect(page).toHaveURL(/\/app\/job\/job_[a-z0-9]+$/)
  const editUrl = `${page.url()}/edit`
  await page.goto(editUrl)
  await expect(page.getByRole("note", { name: "Review Note (read only)" })).toContainText("server")
  await roleHint(page, ["finance"])
  await page.goto(editUrl)
  await expect(page.getByLabel(/^finance only/i)).toBeVisible()
  await expect(page.getByLabel(/^review note/i)).toHaveValue("server")
})

test("typed form validation rejects invalid durations, maps and base64", async ({ page }) => {
  await login(page)
  await page.goto("/app/job/new")
  await page.getByLabel(/^name/i).fill("Invalid values")
  await page.getByLabel(/^review note/i).fill("reviewed")
  await page.getByLabel(/^timeout/i).fill("ninety seconds")
  await page.getByLabel(/^labels/i).fill('{"attempts": "three"}')
  await page.getByLabel(/^checksum/i).fill("not base64!")
  await page.getByRole("button", { name: /^create$/i }).click()
  await expect(page.getByText("Use a duration such as 90s or 1h30m", { exact: true })).toBeVisible()
  await expect(page.getByText("JSON values do not match the field type", { exact: true })).toBeVisible()
  await expect(page.getByText("Use standard padded base64", { exact: true })).toBeVisible()
  await expect(page).toHaveURL(/\/app\/job\/new$/)
})


test("hidden composite children make the whole replacement read-only", async ({ page }) => {
  await login(page)
  await page.goto("/app/job/new")
  await expect(page.getByRole("note", { name: "Hidden Group (read only)" })).toBeVisible()
  await expect(page.locator('input[name="hidden_group.visible"]')).toHaveCount(0)
  const payload = await page.evaluate(async () => {
    const source = "/src/generated/zod-schemas.ts"
    const { normalizeFormPayload } = await import(source)
    const field = {
      leaf: "hidden_group", name: "hidden_group", kind: "composite", item_kind: null,
      required: true, computed: false, derived: false, has_hidden_children: true,
      read_roles: [], write_roles: [], sub_fields: [],
    }
    return normalizeFormPayload({ hidden_group: { visible: "changed" } }, [field], () => {})
  })
  expect(payload).not.toHaveProperty("hidden_group")
})
