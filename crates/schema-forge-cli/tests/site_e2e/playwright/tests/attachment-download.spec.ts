import { expect, test, type Page } from "@playwright/test"
import { readFile } from "node:fs/promises"

async function mountDownload(page: Page) {
  await page.goto("/login")
  await page.evaluate(async () => {
    // The login page has loaded these Vite-prebundled React modules already.
    const reactPath = "/node_modules/.vite/deps/react.js"
    const domPath = "/node_modules/.vite/deps/react-dom_client.js"
    const componentPath = "/src/components/ui/file-upload.tsx"
    const authPath = "/src/lib/auth.ts"
    const React = await import(reactPath)
    const ReactDOM = await import(domPath)
    const { AttachmentDownload } = await import(componentPath)
    const auth = await import(authPath)
    auth.tokenStore.set("download-token", new Date(Date.now() + 3600000).toISOString(), ["member"])
    auth.activeTenantStore.set("Org", "org-selected")
    const host = document.createElement("div")
    host.id = "attachment-fixture"
    document.body.appendChild(host)
    const originalCreate = URL.createObjectURL.bind(URL)
    const originalRevoke = URL.revokeObjectURL.bind(URL)
    URL.createObjectURL = (blob: Blob | MediaSource) => {
      const url = originalCreate(blob)
      host.dataset.createdBlob = url
      return url
    }
    URL.revokeObjectURL = (url: string) => {
      host.dataset.revokedBlob = url
      originalRevoke(url)
    }
    ReactDOM.createRoot(host).render(React.createElement(AttachmentDownload, {
      schema: "Company", entityId: "company_fixture", fieldName: "document", access: "proxied",
      attachment: { key: "company_fixture/original.txt", size: 16, mime: "text/plain", status: "available" },
    }))
  })
}

for (const [disposition, expected] of [
  ["attachment; filename*=UTF-8''quarterly%20r%C3%A9sum%C3%A9.txt", "quarterly résumé.txt"],
  ['attachment; filename="report.txt"', "report.txt"],
  ["", "original.txt"],
]) {
  test(`proxied download carries session context and saves ${expected}`, async ({ page }) => {
    await page.route("**/api/v1/forge/schemas/Company/entities/company_fixture/fields/document", route => {
      expect(route.request().headers().authorization).toBe("Bearer download-token")
      expect(route.request().headers()["x-active-tenant"]).toBe("Org:org-selected")
      return route.fulfill({ status: 200, contentType: "text/plain", body: "private contents", headers: disposition ? { "content-disposition": disposition } : {} })
    })
    await mountDownload(page)
    const pending = page.waitForEvent("download")
    await page.locator("#attachment-fixture").getByRole("button", { name: /Download/ }).click()
    const download = await pending
    expect(download.suggestedFilename()).toBe(expected)
    const path = await download.path()
    expect(path).not.toBeNull()
    expect(await readFile(path!, "utf8")).toBe("private contents")
    await expect.poll(() => page.locator("#attachment-fixture").evaluate(element => {
      const host = element as HTMLElement
      return Boolean(host.dataset.createdBlob) && host.dataset.createdBlob === host.dataset.revokedBlob
    })).toBe(true)
    await expect(page.locator('a[href^="blob:"]')).toHaveCount(0)
  })
}

test("proxied download failure shows an error and allows retry", async ({ page }) => {
  await page.route("**/api/v1/forge/schemas/Company/entities/company_fixture/fields/document", route => route.fulfill({ status: 403, json: { error: "forbidden" } }))
  await mountDownload(page)
  const fixture = page.locator("#attachment-fixture")
  await fixture.getByRole("button", { name: /Download/ }).click()
  await expect(fixture.getByRole("alert")).toContainText("Unable to download this file (HTTP 403).")
  await expect(fixture.getByRole("button", { name: /Download/ })).toBeEnabled()
  await expect(fixture).not.toHaveAttribute("data-created-blob")
})
