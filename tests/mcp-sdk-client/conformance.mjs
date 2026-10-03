import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const repositoryRoot = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const executable = process.env.HYCEL_MCP_BINARY ?? resolve(repositoryRoot, "target/debug/hycel-mcp");
const projectRoot = process.env.HYCEL_PROJECT_ROOT ?? resolve(repositoryRoot, "examples/bellglass-courier");
const transport = new StdioClientTransport({
  command: executable,
  args: ["--project-root", projectRoot],
  stderr: "pipe",
});
const client = new Client(
  { name: "hycel-official-sdk-conformance", version: "0.1.0" },
  { capabilities: {} },
);

try {
  await client.connect(transport);
  assert.equal(client.getServerVersion()?.name, "hycel-project-services");
  assert.equal(client.getServerVersion()?.version, "0.1.0");
  assert.equal(client.getServerCapabilities()?.tools?.listChanged, false);

  const { tools } = await client.listTools();
  const inspectTool = tools.find(({ name }) => name === "project_inspect");
  assert.ok(inspectTool, "read-only inspection tool must be advertised");
  assert.equal(inspectTool.inputSchema.additionalProperties, false);
  assert.ok(tools.some(({ name }) => name === "project_check"));
  assert.ok(tools.some(({ name }) => name === "project_tests"));
  assert.ok(!tools.some(({ name }) => name.endsWith("_apply")), "apply tools must be absent by default");
  assert.ok(!tools.some(({ name }) => name === "scene_screenshot"), "file-writing screenshot tool must be absent by default");

  const inspection = await client.callTool({
    name: "project_inspect",
    arguments: { offset: 0, limit: 2 },
  });
  assert.equal(inspection.isError, false);
  assert.equal(inspection.structuredContent.result.limit, 2);

  const check = await client.callTool({ name: "project_check", arguments: {} });
  assert.equal(check.isError, false);
  assert.equal(check.structuredContent.ok, true);

  const scenarios = await client.callTool({
    name: "project_tests",
    arguments: { scenario: "echo-flight" },
  });
  assert.equal(scenarios.isError, false);
  assert.equal(scenarios.structuredContent.result.tests[0].name, "echo-flight");
  assert.equal(scenarios.structuredContent.result.tests[0].passed, true);

  const temporaryRoot = mkdtempSync(join(tmpdir(), "hycel-mcp-sdk-"));
  const writableRoot = join(temporaryRoot, "project");
  cpSync(projectRoot, writableRoot, { recursive: true });
  const scenePath = join(writableRoot, "scenes", "first-room.json");
  const originalScene = readFileSync(scenePath);
  const writableTransport = new StdioClientTransport({
    command: executable,
    args: ["--project-root", writableRoot, "--allow-writes"],
    stderr: "pipe",
  });
  const writableClient = new Client(
    { name: "hycel-official-sdk-write-conformance", version: "0.1.0" },
    { capabilities: {} },
  );
  try {
    await writableClient.connect(writableTransport);
    const writableTools = await writableClient.listTools();
    assert.ok(writableTools.tools.some(({ name }) => name === "scene_edit_apply"));
    const operation = { operation: "rename_scene", name: "SDK Round Trip" };
    const preview = await writableClient.callTool({
      name: "scene_edit_preview",
      arguments: { scene_file: "scenes/first-room.json", operation },
    });
    assert.equal(preview.isError, false);
    assert.deepEqual(readFileSync(scenePath), originalScene);
    const token = preview.structuredContent.preview_token;
    assert.equal(typeof token, "string");

    const applied = await writableClient.callTool({
      name: "scene_edit_apply",
      arguments: { scene_file: "scenes/first-room.json", operation, preview_token: token },
    });
    assert.equal(applied.isError, false);
    assert.match(readFileSync(scenePath, "utf8"), /SDK Round Trip/);
    assert.deepEqual(readFileSync(join(writableRoot, applied.structuredContent.backup_path)), originalScene);

    const replayedToken = await writableClient.callTool({
      name: "scene_edit_apply",
      arguments: { scene_file: "scenes/first-room.json", operation, preview_token: token },
    });
    assert.equal(replayedToken.isError, true, "a preview token must be single-use");
  } finally {
    await writableClient.close();
    rmSync(temporaryRoot, { recursive: true, force: true });
  }

  console.log("Official MCP TypeScript SDK 1.30.0 stdio smoke passed.");
} finally {
  await client.close();
}
