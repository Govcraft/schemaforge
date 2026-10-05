import { expect, test, type Page } from "@playwright/test"

async function session(page: Page) {
  await page.addInitScript(() => {
    sessionStorage.setItem("schemaforge.token", "relation-test-token")
    sessionStorage.setItem("schemaforge.token_expires_at", new Date(Date.now() + 60 * 60 * 1000).toISOString())
    sessionStorage.setItem("schemaforge.roles", JSON.stringify(["member"]))
  })
  await page.route("**/api/v1/forge/auth/me", route => route.fulfill({ json: {
    user_id: "member", roles: ["member"], tenant_chain: [], active_tenant: null,
  } }))
  await page.route("**/api/v1/forge/schemas", route => route.fulfill({ json: {
    schemas: ["PrimaryJoin", "ExplicitJoin", "ManyJoin", "JoinReference"].map(name => ({
      name, annotations: [], permissions: { create: true },
    })), count: 4,
  } }))
}

for (const [schema, path] of [["PrimaryJoin", "primary-join"], ["ExplicitJoin", "explicit-join"]]) {
  test(`${schema} primary relation label links to its own row with an ID fallback`, async ({ page }) => {
    await session(page)
    await page.route(`**/api/v1/forge/schemas/${schema}/entities**`, route => route.fulfill({ json: {
      entities: [
        { id: "join_one", schema, fields: { company: "company_one", company__display: "Blue widget" } },
        { id: "join_two", schema, fields: { company: "company_two" } },
      ], count: 2, permissions: { create: true },
    } }))
    await page.goto(`/app/${path}`)
    await expect(page.getByRole("link", { name: "Blue widget", exact: true })).toHaveAttribute("href", `/app/${path}/join_one`)
    await expect(page.getByRole("link", { name: "company_two", exact: true })).toHaveAttribute("href", `/app/${path}/join_two`)
  })
}

test("many primary relation labels retain per-item ID fallbacks", async ({ page }) => {
  await session(page)
  await page.route("**/api/v1/forge/schemas/ManyJoin/entities**", route => route.fulfill({ json: {
    entities: [{ id: "join_many", schema: "ManyJoin", fields: {
      companies: ["company_one", "company_two"], companies__display: ["Blue widget", null],
    } }], count: 1, permissions: { create: true },
  } }))
  await page.goto("/app/many-join")
  await expect(page.getByRole("link", { name: "Blue widget, company_two", exact: true })).toHaveAttribute("href", "/app/many-join/join_many")
})

test("relation picker prefers the display companion of a relation-valued display field", async ({ page }) => {
  await session(page)
  await page.route("**/api/v1/forge/schemas/PrimaryJoin/entities**", route => route.fulfill({ json: {
    entities: [
      { id: "join_one", schema: "PrimaryJoin", fields: { company: "company_one", company__display: "Blue widget" } },
      { id: "join_two", schema: "PrimaryJoin", fields: { company: "company_two" } },
    ], count: 2,
  } }))
  await page.goto("/app/join-reference/new")
  const picker = page.getByRole("combobox", { name: /^entry/i })
  await expect(picker.locator('option[value="join_one"]')).toHaveText("Blue widget")
  await expect(picker.locator('option[value="join_two"]')).toHaveText("company_two")
  await picker.selectOption("join_one")
  await expect(picker).toHaveValue("join_one")
})
