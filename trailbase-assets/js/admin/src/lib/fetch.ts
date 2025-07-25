import { computed } from "nanostores";
import { persistentAtom } from "@nanostores/persistent";
import { Client, type Tokens, type User } from "trailbase";

import { showToast } from "@/components/ui/toast";

import type { QueryResponse } from "@bindings/QueryResponse";
import type { QueryRequest } from "@bindings/QueryRequest";

const $tokens = persistentAtom<Tokens | null>("auth_tokens", null, {
  encode: JSON.stringify,
  decode: JSON.parse,
});
export const $user = computed($tokens, (_tokens) => client.user());

function initClient(): Client {
  // For our dev server setup we assume that a TrailBase instance is running at ":4000", otherwise
  // we query APIs relative to the origin's root path.
  const HOST = import.meta.env.DEV
    ? new URL("http://localhost:4000")
    : undefined;
  const client = Client.init(HOST, {
    tokens: $tokens.get() ?? undefined,
    onAuthChange: (c: Client, _user: User | undefined) => {
      $tokens.set(c.tokens() ?? null);
    },
  });

  // This will also trigger a logout in case of 401.
  client.refreshAuthToken();

  return client;
}
export const client = initClient();

type FetchOptions = RequestInit & {
  throwOnError?: boolean;
};

export async function adminFetch(
  input: string,
  init?: FetchOptions,
): Promise<Response> {
  if (!input.startsWith("/")) {
    throw Error("Should start with '/'");
  }

  try {
    return await client.fetch(`/api/_admin${input}`, {
      headers: {
        "Content-Type": "application/json",
      },
      ...init,
    });
  } catch (err) {
    showToast({
      title: "Fetch Error",
      description: `${err}`,
      variant: "error",
    });

    throw err;
  }
}

export type ExecutionError = {
  code: number;
  message: string;
};

export type ExecutionResult = {
  query: string;
  timestamp: number;

  data?: QueryResponse;
  error?: ExecutionError;
};

export async function executeSql(sql: string): Promise<ExecutionResult> {
  const response = await adminFetch("/query", {
    method: "POST",
    body: JSON.stringify({
      query: sql,
    } as QueryRequest),
    throwOnError: false,
  });

  if (response.ok) {
    return {
      query: sql,
      timestamp: Date.now(),
      data: await response.json(),
    } as ExecutionResult;
  }

  return {
    query: sql,
    timestamp: Date.now(),
    error: {
      code: response.status,
      message: await response.text(),
    } as ExecutionError,
  } as ExecutionResult;
}
