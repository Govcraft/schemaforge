import { expect, test, type Page, type Route } from "@playwright/test"

async function session(page: Page, entityResponse?: (route: Route) => Promise<void>) {
  await page.addInitScript(() => {
    // Stay below the browser timer limit so background refresh does not fire during setup.
    sessionStorage.setItem("schemaforge.token", "test-token")
    sessionStorage.setItem("schemaforge.token_expires_at", new Date(Date.now() + 60 * 60 * 1000).toISOString())
    sessionStorage.setItem("schemaforge.roles", JSON.stringify(["member"]))
    if (!sessionStorage.getItem("schemaforge.active_tenant")) {
      sessionStorage.setItem("schemaforge.active_tenant", "Org:org-a")
    }
  })
  await page.route("**/api/v1/forge/auth/me", async route => {
    const active = route.request().headers()["x-active-tenant"]
    await route.fulfill({ json: {
      user_id: "member", email: "member@example.com", display_name: "Member", roles: ["member"],
      tenant_chain: [{ schema: "Org", entity_id: "org-a" }, { schema: "Org", entity_id: "org-b" }],
      active_tenant: active ? { tenant_type: "Org", tenant_id: active.split(":")[1] } : null,
    } })
  })
  await page.route("**/api/v1/forge/schemas", route => route.fulfill({ json: {
    schemas: [{ name: "Company", annotations: [], permissions: { create: true } }], count: 1,
  } }))
  await page.route("**/api/v1/forge/schemas/Company/entities**", async route => {
    if (entityResponse) return entityResponse(route)
    const tenant = route.request().headers()["x-active-tenant"]
    if (!tenant) return route.fulfill({ status: 400, json: { code: "ACTIVE_TENANT_REQUIRED" } })
    return route.fulfill({ json: {
      entities: [{ id: "company_test", schema: "Company", fields: { name: `Company for ${tenant}`, status: "active" } }],
      count: 1, permissions: { create: true },
    } })
  })
}

test("multi-membership tenant picker scopes the entity list after switching", async ({ page }) => {
  await session(page)
  await page.goto("/app/company")
  await expect(page.getByText("Company for Org:org-a", { exact: true })).toBeVisible()
  await page.getByRole("combobox", { name: "Switch active tenant" }).selectOption("Org:org-b")
  await expect(page.getByText("Company for Org:org-b", { exact: true })).toBeVisible()
  await expect(page.getByText("Company for Org:org-a", { exact: true })).toHaveCount(0)
})

test("request headers preserve explicit tenant overrides and refresh retries", async ({ page }) => {
  await page.goto("/login")
  let retries = 0
  await page.route("**/api/v1/forge/auth/refresh", route => route.fulfill({ json: {
    token: "new-token", expires_at: new Date(Date.now() + 60 * 60 * 1000).toISOString(), roles: ["member"],
  } }))
  await page.route("**/api/v1/forge/header-test**", async route => {
    if (route.request().url().endsWith("/retry") && retries++ === 0) {
      return route.fulfill({ status: 401, json: { error: "unauthorized" } })
    }
    return route.fulfill({ json: route.request().headers() })
  })
  const headers = await page.evaluate(async () => {
    const authPath = "/src/lib/auth.ts"
    const apiPath = "/src/generated/api-client.ts"
    const auth = await import(authPath)
    const api = await import(apiPath)
    auth.tokenStore.set("old-token", new Date(Date.now() + 60 * 60 * 1000).toISOString(), ["member"])
    auth.activeTenantStore.set("Org", "org-a")
    const result = []
    for (const method of ["GET", "POST", "PATCH", "DELETE"]) {
      result.push(await api.request("/api/v1/forge/header-test", { method }))
    }
    result.push(await api.request("/api/v1/forge/header-test", {
      headers: new Headers({ "x-active-tenant": "Org:explicit", "X-Extra": "preserved" }),
    }))
    result.push(await api.request("/api/v1/forge/header-test", {
      headers: [["x-active-tenant", "Org:tuple"]],
    }))
    auth.activeTenantStore.set("Org", "org-b")
    result.push(await api.request("/api/v1/forge/header-test/retry"))
    auth.activeTenantStore.clear()
    result.push(await api.request("/api/v1/forge/header-test"))
    return result
  })
  for (const value of headers.slice(0, 4)) expect(value["x-active-tenant"]).toBe("Org:org-a")
  expect(headers[4]["x-active-tenant"]).toBe("Org:explicit")
  expect(headers[4]["x-extra"]).toBe("preserved")
  expect(headers[5]["x-active-tenant"]).toBe("Org:tuple")
  expect(headers[6]["x-active-tenant"]).toBe("Org:org-b")
  expect(headers[6].authorization).toBe("Bearer new-token")
  expect(headers[7]["x-active-tenant"]).toBeUndefined()
  expect(retries).toBe(2)
})

test("API errors use readable envelope messages and status fallbacks", async ({ page }) => {
  await page.goto("/login")
  const messages = await page.evaluate(async () => {
    const path = "/src/generated/api-client.ts"
    const { ApiError, formatApiError } = await import(path)
    return [
      formatApiError(new ApiError(422, { error: "validation_failed", message: "A title is required" }, "raw")),
      formatApiError(new ApiError(401, { error: "This token is expired", code: "INVALID_TOKEN" }, "raw")),
      formatApiError(new ApiError(403, { error: "forbidden" }, "raw")),
      formatApiError(new ApiError(429, undefined, "<html>proxy error</html>")),
      formatApiError(new TypeError("Failed to fetch")),
      formatApiError({ arbitrary: "object" }),
    ]
  })
  expect(messages).toEqual([
    "A title is required", "This token is expired", "You don't have permission to do that.",
    "Too many requests. Try again shortly.", "Unable to connect. Check your connection and try again.",
    "Something went wrong. Please try again.",
  ])
})

test("unique save errors stay inline without a global toast", async ({ page }) => {
  await session(page, route => route.fulfill({ status: 409, json: {
    error: "unique_violation", field: "name", message: "This name is already in use",
  } }))
  await page.goto("/app/company/new")
  await page.getByLabel(/^name/i).fill("Duplicate")
  await page.getByRole("button", { name: /^create$/i }).click()
  await expect(page.getByText("Already in use", { exact: true })).toBeVisible()
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0)
})

test("save errors show one readable local toast", async ({ page }) => {
  await session(page, route => route.fulfill({ status: 422, json: {
    error: "validation_failed", message: "Company name must contain a letter",
  } }))
  await page.goto("/app/company/new")
  await page.getByLabel(/^name/i).fill("123")
  await page.getByRole("button", { name: /^create$/i }).click()
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(1)
  await expect(page.locator("[data-sonner-toast]")).toContainText("Company name must contain a letter")
})

test("list load errors render recovery inline without a global toast", async ({ page }) => {
  await session(page, route => route.fulfill({ status: 403, json: { error: "forbidden" } }))
  await page.goto("/app/company")
  await expect(page.getByRole("alert")).toContainText("You don't have permission to do that.", { timeout: 15_000 })
  await expect(page.getByRole("button", { name: "Retry", exact: true })).toBeVisible()
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0)
})


test("customizable global handlers honor query and mutation opt-outs", async ({ page }) => {
  await page.goto("/login")
  await page.evaluate(async () => {
    const path = "/src/lib/error-toast.ts"
    const handlers = await import(path)
    const error = new Error("Handled locally")
    handlers.onQueryError(error, { meta: { suppressGlobalError: true } })
    handlers.onMutationError(error, undefined, undefined, { meta: { suppressGlobalError: true } })
    handlers.onMutationError(error, undefined, undefined, { options: { onError: () => {} } })
  })
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(0)
  await page.evaluate(async () => {
    const path = "/src/lib/error-toast.ts"
    const handlers = await import(path)
    handlers.onQueryError(new Error("Global load failure"), {})
  })
  await expect(page.locator("[data-sonner-toast]")).toHaveCount(1)
  await expect(page.locator("[data-sonner-toast]")).toContainText("Global load failure")
})
