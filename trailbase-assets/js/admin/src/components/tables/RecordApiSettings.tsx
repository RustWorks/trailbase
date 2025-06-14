import { For, JSXElement, createSignal } from "solid-js";
import { createForm } from "@tanstack/solid-form";
import { TbInfoCircle } from "solid-icons/tb";
import { useQueryClient } from "@tanstack/solid-query";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardTitle, CardHeader } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SheetFooter } from "@/components/ui/sheet";
import { showToast } from "@/components/ui/toast";

import {
  Config,
  ConflictResolutionStrategy,
  PermissionFlag,
  RecordApiConfig,
} from "@proto/config";
import { SheetContainer } from "@/components/SafeSheet";
import {
  buildTextFormField,
  buildOptionalTextFormField,
} from "@/components/FormFields";
import { createConfigQuery, setConfig } from "@/lib/config";
import { parseSqlExpression } from "@/lib/parse";
import { tableType, getForeignKey } from "@/lib/schema";

import type { ForeignKey } from "@bindings/ForeignKey";
import type { Table } from "@bindings/Table";
import type { View } from "@bindings/View";

const tablePermissions = {
  Create: PermissionFlag.CREATE,
  Read: PermissionFlag.READ,
  Update: PermissionFlag.UPDATE,
  Delete: PermissionFlag.DELETE,
  Schema: PermissionFlag.SCHEMA,
} as const;

const viewPermissions = {
  Read: PermissionFlag.READ,
  Schema: PermissionFlag.SCHEMA,
} as const;

async function asyncSqlValidator({ value }: { value: string | undefined }) {
  console.debug("Query", value);
  if (value) {
    return parseSqlExpression(value);
  }
}

function AclForm(props: {
  entity: string;
  initial?: PermissionFlag[];
  showHeader: boolean;
  onChange: (v: PermissionFlag[]) => void;
  view: boolean;
}) {
  const [acl, setAcl] = createSignal(new Set(props.initial ?? []));

  return (
    <div class="flex">
      <div
        class="grid w-[300px] items-end gap-2"
        style={{ "grid-template-columns": "auto 1fr 1fr 1fr 1fr 1fr" }}
      >
        {props.showHeader && (
          <For
            each={Object.keys(props.view ? viewPermissions : tablePermissions)}
          >
            {(key, index) => (
              <div
                class="col-span-1 ml-1 [writing-mode:vertical-rl]"
                style={{ "grid-column-start": index() + 2 }}
              >
                {key}
              </div>
            )}
          </For>
        )}

        <div class="col-span-1 col-start-1 w-[120px]">
          <Label>{props.entity}</Label>
        </div>

        <For
          each={Object.values(props.view ? viewPermissions : tablePermissions)}
        >
          {(perm) => (
            <div class="col-span-1">
              <Checkbox
                checked={acl().has(perm)}
                onChange={(v: boolean) => {
                  const set = acl();
                  if (v) {
                    set.add(perm);
                  } else {
                    set.delete(perm);
                  }

                  setAcl(new Set(set));
                  props.onChange([...set]);
                }}
              />
            </div>
          )}
        </For>
      </div>
    </div>
  );
}

type Field = keyof RecordApiConfig;
interface AccessRule {
  field: Field;
  label: string;
  description: string;
}

const tableAccessRules: AccessRule[] = [
  {
    field: "readAccessRule",
    label: "Read Access:",
    description:
      'Row- and request-level read access (_user_, _row_, _req_): If the table has an "owner"\'s column containing binary user ids, access could be rstricted to the owner by setting \'_row_.owner = _user_\' here. Or if the table as a foreign key to a "group" and a relationship defined in a "membership" table: \'(SELECT 1 FROM membership WHERE group = _row_.group AND user = _user_)\'',
  },
  {
    field: "createAccessRule",
    label: "Create Access:",
    description:
      "Request-level create access validation base on _USER_, _REQ_:",
  },
  {
    field: "updateAccessRule",
    label: "Update Access",
    description:
      "Row- and request level update access based on _USER_, _ROW_, _REQ_:",
  },
  {
    field: "deleteAccessRule",
    label: "Delete Access",
    description:
      "Row- and request level delete access based on _USRE_, _ROW_, _REQ_:",
  },
  {
    field: "schemaAccessRule",
    label: "Schema Access",
    description: "Schema access based on _USER_:",
  },
] as const;

const viewAccessRules: AccessRule[] = [
  {
    field: "readAccessRule",
    label: "Read access:",
    description:
      'Row- and request-level read access (_user_, _row_, _req_): If the table has an "owner"\'s column containing binary user ids, access could be rstricted to the owner by setting \'_row_.owner = _user_\' here. Or if the table as a foreign key to a "group" and a relationship defined in a "membership" table: \'(SELECT 1 FROM membership WHERE group = _row_.group AND user = _user_)\'',
  },
  {
    field: "schemaAccessRule",
    label: "Schema Access",
    description: "Schema access based on _USER_:",
  },
] as const;

function updateRecordApiConfig(
  config: Config,
  recordApiConfig: RecordApiConfig,
): Config {
  const newConfig = Config.fromPartial(config);

  for (const i in newConfig.recordApis) {
    const api = newConfig.recordApis[i];
    if (api.name == recordApiConfig.name) {
      newConfig.recordApis[i] = recordApiConfig;
      return newConfig;
    }
  }

  newConfig.recordApis.push(recordApiConfig);
  return newConfig;
}

function removeRecordApiConfig(config: Config, tableName: string): Config {
  const newConfig = Config.fromPartial(config);

  while (true) {
    const index = newConfig.recordApis.findIndex(
      (api) => api.tableName === tableName,
    );
    if (index < 0) {
      break;
    }

    newConfig.recordApis.splice(index, 1);
  }

  return newConfig;
}

function ConflictResolutionSrategyToString(
  value: ConflictResolutionStrategy | null,
): string {
  switch (value) {
    case ConflictResolutionStrategy.ABORT:
      return "Abort";
    case ConflictResolutionStrategy.ROLLBACK:
      return "Rollback";
    case ConflictResolutionStrategy.FAIL:
      return "Fail";
    case ConflictResolutionStrategy.IGNORE:
      return "Ignore";
    case ConflictResolutionStrategy.REPLACE:
      return "Replace";
    default:
      return "Undefined";
  }
}

export function getRecordApis(
  config: Config | undefined,
  tableName: string,
): RecordApiConfig[] {
  return (config?.recordApis ?? []).filter(
    (api) => api.tableName === tableName,
  );
}

export function hasRecordApis(
  config: Config | undefined,
  tableName: string,
): boolean {
  for (const api of config?.recordApis ?? []) {
    if (api.tableName === tableName) {
      return true;
    }
  }
  return false;
}

function findRecordApi(
  config: Config | undefined,
  tableName: string,
): RecordApiConfig | undefined {
  const apis = getRecordApis(config, tableName);

  switch (apis.length) {
    case 0:
      return undefined;
    case 1:
      return apis[0];
    default:
      console.warn("Multiple APIs not yet supported in UI, picking first.");
      return apis[0];
  }
}

function StyledHoverCard(props: { children: JSXElement }) {
  return (
    <HoverCard>
      <HoverCardTrigger
        class="size-[32px]"
        as={Button<"button">}
        variant="link"
      >
        <TbInfoCircle />
      </HoverCardTrigger>

      <HoverCardContent class="w-80">{props.children}</HoverCardContent>
    </HoverCard>
  );
}

function getForeignKeyColumns(schema: Table | View): [string, ForeignKey][] {
  function filter([colName, fk]: [string, ForeignKey | undefined]) {
    if (!fk) {
      return false;
    }

    if (colName.startsWith("_")) {
      return false;
    }

    if (fk.foreign_table.startsWith("_")) {
      return false;
    }

    return true;
  }

  return (schema.columns ?? [])
    .map(
      (c) =>
        [c.name, getForeignKey(c.options)] as [string, ForeignKey | undefined],
    )
    .filter(filter) as [string, ForeignKey][];
}

export function RecordApiSettingsForm(props: {
  close: () => void;
  markDirty: () => void;
  schema: Table | View;
}) {
  const queryClient = useQueryClient();
  const config = createConfigQuery();

  const type = () => tableType(props.schema);
  const foreignKeys = () => getForeignKeyColumns(props.schema);

  // FIXME: We don't currently handle the "multiple APIs for a single table" case.
  const currentApi = () =>
    findRecordApi(config.data!.config, props.schema.name.name);

  const form = createForm(() => {
    const tableName = props.schema.name.name;
    return {
      defaultValues:
        currentApi() ??
        ({
          name: tableName,
          tableName: tableName,
          aclWorld: [],
          aclAuthenticated: [],
          excludedColumns: [],
          expand: [],
        } as RecordApiConfig),
      onSubmit: async ({ value }: { value: RecordApiConfig }) => {
        console.debug("Add record api config:", value);

        const c = config.data?.config;
        if (!c) {
          console.error("missing base configuration");
          return;
        }

        const newConfig = updateRecordApiConfig(c, value);
        try {
          await setConfig(queryClient, newConfig);
          props.close();
        } catch (err) {
          showToast({
            title: "Uncaught Error",
            description: `${err}`,
            variant: "error",
          });
        }
      },
    };
  });

  form.useStore((state) => {
    if (state.isDirty && !state.isSubmitted) {
      props.markDirty();
    }
  });

  return (
    <SheetContainer>
      <form
        method="dialog"
        onSubmit={(e: SubmitEvent) => {
          e.preventDefault();
          form.handleSubmit();
        }}
      >
        <div class="flex flex-col items-start gap-4 py-4">
          <Card class="w-full">
            <CardHeader>
              <CardTitle>Record API Settings</CardTitle>
            </CardHeader>

            <CardContent class="my-4 flex flex-col gap-4">
              <form.Field
                name="name"
                validators={{
                  onChange: ({ value }: { value: string | undefined }) => {
                    return value ? undefined : "Api name missing";
                  },
                }}
              >
                {buildTextFormField({
                  label: () => (
                    <div class={labelWidth}>
                      <Label>API Name</Label>
                      <StyledHoverCard>
                        <div class="flex justify-between space-x-4">
                          <div class="space-y-1 text-sm">
                            Public name used to access the API via{" "}
                            <span class="font-mono">/api/records/v1/name</span>.
                          </div>
                        </div>
                      </StyledHoverCard>
                    </div>
                  ),
                })}
              </form.Field>

              {type() === "table" && (
                <>
                  <form.Field name="conflictResolution">
                    {(field) => (
                      <div class="flex items-center justify-between gap-2">
                        <div>
                          <Label>Conflict Resolution</Label>
                          <StyledHoverCard>
                            <div class="flex justify-between space-x-4">
                              <div class="space-y-1 text-sm">
                                SQLite conflict resolution strategy to employ on
                                record collision.
                              </div>
                            </div>
                          </StyledHoverCard>
                        </div>

                        <Select
                          multiple={false}
                          placeholder="Select group..."
                          defaultValue={field().state.value}
                          options={[
                            ConflictResolutionStrategy.CONFLICT_RESOLUTION_STRATEGY_UNDEFINED,
                            ConflictResolutionStrategy.ABORT,
                            ConflictResolutionStrategy.ROLLBACK,
                            ConflictResolutionStrategy.FAIL,
                            ConflictResolutionStrategy.IGNORE,
                            ConflictResolutionStrategy.REPLACE,
                          ]}
                          onChange={(
                            strategy: ConflictResolutionStrategy | null,
                          ) => {
                            if (
                              strategy === null ||
                              strategy ==
                                ConflictResolutionStrategy.CONFLICT_RESOLUTION_STRATEGY_UNDEFINED
                            ) {
                              field().handleChange(undefined);
                            } else {
                              field().handleChange(strategy);
                            }
                          }}
                          itemComponent={(props) => (
                            <SelectItem item={props.item}>
                              {ConflictResolutionSrategyToString(
                                props.item.rawValue,
                              )}
                            </SelectItem>
                          )}
                        >
                          <SelectTrigger class="w-[180px]">
                            <SelectValue<ConflictResolutionStrategy>>
                              {(state) =>
                                ConflictResolutionSrategyToString(
                                  state.selectedOption(),
                                )
                              }
                            </SelectValue>
                          </SelectTrigger>

                          <SelectContent />
                        </Select>
                      </div>
                    )}
                  </form.Field>

                  <form.Field
                    name="autofillMissingUserIdColumns"
                    children={(field) => {
                      // TODO: Should be buildBoolFormField?
                      const v = () => field().state.value;
                      return (
                        <div class="mt-2 flex items-center justify-between gap-2">
                          <div>
                            <Label>Infer Missing User</Label>
                            <StyledHoverCard>
                              <div class="flex justify-between space-x-4">
                                <div class="space-y-1">
                                  <p class="text-sm">
                                    When enabled, user id values not provided as
                                    part of a CREATE request will be auto-filled
                                    using the calling user's authentication
                                    context.
                                  </p>

                                  <p class="text-sm">
                                    For most use-cases this setting should be
                                    off with user ids being provided explicitly
                                    by the client. This can be useful for static
                                    HTML forms.
                                  </p>
                                </div>
                              </div>
                            </StyledHoverCard>
                          </div>

                          <Checkbox
                            checked={v()}
                            onChange={(v: boolean) => field().handleChange(v)}
                          />
                        </div>
                      );
                    }}
                  />

                  <form.Field
                    name="enableSubscriptions"
                    children={(field) => {
                      const v = () => field().state.value;
                      return (
                        <div class="mt-2 flex items-center justify-between gap-2">
                          <div>
                            <Label>Enable Subscriptions</Label>
                            <StyledHoverCard>
                              <div class="flex justify-between space-x-4">
                                <div class="space-y-1">
                                  <p class="text-sm">
                                    When enabled, users can subscribe to data
                                    changes in real time. Record access is
                                    checked on a per-record level at
                                    notification time ensuring up-to-date
                                    enforcement as data evolves.
                                  </p>
                                </div>
                              </div>
                            </StyledHoverCard>
                          </div>

                          <Checkbox
                            checked={v()}
                            onChange={(v: boolean) => field().handleChange(v)}
                          />
                        </div>
                      );
                    }}
                  />

                  <form.Field name="expand">
                    {(field) => {
                      const has = (colName: string) =>
                        new Set([...field().state.value]).has(colName);
                      const add = (colName: string) =>
                        field().handleChange(
                          Array.from(
                            new Set([colName, ...field().state.value]),
                          ),
                        );
                      const remove = (colName: string) => {
                        const s = new Set(field().state.value);
                        s.delete(colName);
                        field().handleChange(Array.from(s));
                      };

                      return (
                        <For each={foreignKeys()}>
                          {([colName, item]) => {
                            return (
                              <div class="mt-2 flex items-center justify-between gap-2">
                                <div>
                                  <Label>
                                    Expand Column ({colName} {"=>"}{" "}
                                    {item.foreign_table})
                                  </Label>
                                  <StyledHoverCard>
                                    <div class="flex justify-between space-x-4">
                                      <div class="space-y-1 text-sm">
                                        Expanding a foreign key column, changes
                                        the APIs field schema from simply being
                                        the foreign key, to
                                        <span class="font-mono">{`{ id: any, data?: object }`}</span>
                                        . Then the respective foreign record can
                                        be included during read/list by
                                        specifying
                                        <span class="font-mono">
                                          ?expand={colName}
                                        </span>
                                        .
                                      </div>
                                    </div>
                                  </StyledHoverCard>
                                </div>

                                <Checkbox
                                  checked={has(colName)}
                                  onChange={(v: boolean) =>
                                    v ? add(colName) : remove(colName)
                                  }
                                />
                              </div>
                            );
                          }}
                        </For>
                      );
                    }}
                  </form.Field>
                </>
              )}
            </CardContent>
          </Card>

          <Card class="w-full">
            <CardHeader>
              <CardTitle>Access</CardTitle>
            </CardHeader>

            <CardContent class="my-4 flex flex-col gap-4">
              <form.Field name="aclWorld">
                {(field) => {
                  const v = field().state.value;
                  return (
                    <div class="mb-4">
                      <AclForm
                        entity="World"
                        showHeader={true}
                        initial={v}
                        onChange={field().handleChange}
                        view={type() === "view"}
                      />
                    </div>
                  );
                }}
              </form.Field>

              <form.Field name="aclAuthenticated">
                {(field) => {
                  const v = field().state.value;
                  return (
                    <div class="mb-4">
                      <AclForm
                        entity="Authenticated"
                        showHeader={false}
                        initial={v}
                        onChange={field().handleChange}
                        view={type() === "view"}
                      />
                    </div>
                  );
                }}
              </form.Field>

              <For
                each={type() === "view" ? viewAccessRules : tableAccessRules}
              >
                {(item) => {
                  return (
                    <form.Field
                      name={item.field}
                      validators={{
                        onChangeAsync: asyncSqlValidator,
                        onChangeAsyncDebounceMs: 500,
                      }}
                    >
                      {buildOptionalTextFormField({
                        label: () => <div class={labelWidth}>{item.label}</div>,
                      })}
                    </form.Field>
                  );
                }}
              </For>
            </CardContent>
          </Card>
        </div>

        <SheetFooter>
          <Button
            disabled={currentApi() === undefined}
            variant="destructive"
            onClick={() => {
              const tableName = props.schema.name;
              console.debug("Remove record API config for:", tableName);

              const c = config.data?.config;
              if (!c) {
                console.error("missing base configuration");
                return;
              }

              const newConfig = removeRecordApiConfig(c, tableName.name);
              setConfig(queryClient, newConfig)
                // eslint-disable-next-line solid/reactivity
                .then(() => props.close())
                .catch(console.error);
            }}
          >
            Disable
          </Button>

          <form.Subscribe
            selector={(state) => ({
              canSubmit: state.canSubmit,
              isSubmitting: state.isSubmitting,
            })}
          >
            {(state) => (
              <Button
                type="submit"
                disabled={!state().canSubmit}
                variant="default"
              >
                {currentApi() ? "Update" : "Enable"}
              </Button>
            )}
          </form.Subscribe>
        </SheetFooter>
      </form>
    </SheetContainer>
  );
}

const labelWidth = "w-[112px]";
