import { Switch, Match, Index } from "solid-js";
import { createForm } from "@tanstack/solid-form";
import { useQueryClient, useQuery } from "@tanstack/solid-query";
import { TbPlayerPlay, TbInfoCircle } from "solid-icons/tb";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { IconButton } from "@/components/IconButton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { TextField, TextFieldInput } from "@/components/ui/text-field";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";

import { type FieldApiT, FieldInfo } from "@/components/FormFields";
import { Config, JobsConfig, SystemJob } from "@proto/config";
import { createConfigQuery, setConfig } from "@/lib/config";
import { listJobs, runJob } from "@/lib/jobs";
import type { Job } from "@bindings/Job";

const cronRegex =
  /^(@(yearly|monthly|weekly|daily|hourly|))|((((\d+,)+\d+|(\d+(\/|-)\d+)|\d+|\*)\s*){6,7})$/;

function isValidCronSpec() {
  return {
    onChange: ({ value }: { value: string }): string | undefined => {
      const matches = cronRegex.test(value);
      if (!matches) {
        return `Not a valid cron spec`;
      }
    },
  };
}

type JobProxy = {
  /// Set to false if the loaded config contained the job.
  default: boolean;
  initialConfig: SystemJob;
  config: SystemJob;
  job?: Job;
};

type FormProxy = {
  jobs: JobProxy[];
};

// function trimDuplicateWhitespaces(s: string) : string {
//   return s.trim().replace(/\s+/g, " ");
// }

function equal(a: SystemJob, b: SystemJob): boolean {
  return (
    a.disabled === b.disabled && a.schedule === b.schedule && a.id === b.id
  );
}

function buildFormProxy(
  config: JobsConfig | undefined,
  jobs: Job[],
): FormProxy {
  const result = new Map<number, JobProxy>();
  if (config) {
    for (const job of config.systemJobs) {
      const id = job.id;
      if (id) {
        result.set(id, {
          default: false,
          initialConfig: job,
          config: { ...job },
        });
      }
    }
  }

  for (const job of jobs) {
    const d: SystemJob = {
      id: job.id,
      schedule: job.schedule,
      disabled: !job.enabled,
    };

    const entry: JobProxy = result.get(job.id) ?? {
      default: true,
      initialConfig: d,
      config: { ...d },
    };

    entry.job = job;
    result.set(job.id, entry);
  }

  const compare = (a: JobProxy, b: JobProxy) =>
    (a.config.id ?? 0) - (b.config.id ?? 0);

  return { jobs: [...result.values()].sort(compare) };
}

function extractConfig(proxy: FormProxy): JobsConfig {
  const systemJobs: SystemJob[] = [];

  for (const entry of proxy.jobs) {
    // Only add entries that were part of the original config or have changed from the initial default.
    if (entry.default === false) {
      systemJobs.push(entry.config);
    } else if (!equal(entry.initialConfig, entry.config)) {
      systemJobs.push(entry.config);
    }
  }

  return {
    systemJobs,
  };
}

function JobSettingsImpl(props: {
  markDirty: () => void;
  postSubmit: () => void;
  config: Config;
  jobs: Job[];
  refetchJobs: () => void;
}) {
  const queryClient = useQueryClient();
  const form = createForm(() => ({
    defaultValues: buildFormProxy(props.config.jobs, props.jobs),
    onSubmit: async ({ value }: { value: FormProxy }) => {
      const jobs = extractConfig(value);
      const newConfig = {
        ...props.config,
        jobs,
      } satisfies Config;

      await setConfig(queryClient, newConfig);
      props.refetchJobs();
      props.postSubmit();
    },
  }));

  form.useStore((state) => {
    if (state.isDirty && !state.isSubmitted) {
      props.markDirty();
    }
  });

  return (
    <form
      method="dialog"
      onSubmit={(e: SubmitEvent) => {
        e.preventDefault();
        form.handleSubmit();
      }}
    >
      <Card>
        <CardHeader>
          <h2>Periodic Jobs</h2>
        </CardHeader>

        <CardContent>
          <Table>
            <TableHeader>
              {/*
              <TableHead>Id</TableHead>
              */}
              <TableHead>Name</TableHead>
              <TableHead>
                <Tooltip>
                  <TooltipTrigger as="div">
                    <div class="flex items-center gap-2">
                      Schedule <TbInfoCircle />
                    </div>
                  </TooltipTrigger>

                  <TooltipContent>
                    <p>6/7-component cron spec:</p>
                    <p class="break-keep font-bold">
                      second minute hour day-of-month month day-of-week [year]
                    </p>
                  </TooltipContent>
                </Tooltip>
              </TableHead>
              <TableHead>Next Run</TableHead>
              <TableHead>Last Run</TableHead>
              <TableHead>Enabled</TableHead>
              <TableHead>Action</TableHead>
            </TableHeader>

            <TableBody>
              <form.Field name="jobs" mode="array">
                {(field) => (
                  <Index each={field().state.value}>
                    {(proxy: () => JobProxy, i: number) => {
                      const next = () => {
                        const timestamp = proxy().job?.next;
                        if (!timestamp) return null;

                        const t = new Date(Number(timestamp) * 1000);

                        return (
                          <Tooltip>
                            <TooltipTrigger as="div">
                              <div class="flex items-center gap-2">
                                <TbInfoCircle />
                                <div class="w-[128px] text-sm">
                                  {t.toUTCString()}
                                </div>
                              </div>
                            </TooltipTrigger>

                            <TooltipContent>
                              {t.toLocaleString()} (Local)
                            </TooltipContent>
                          </Tooltip>
                        );
                      };

                      const latest = () => {
                        const latest = proxy().job?.latest;
                        if (!latest) return null;

                        const [startTimestamp, durationMillis, error] = latest;
                        const t = new Date(Number(startTimestamp) * 1000);

                        return (
                          <div
                            classList={{
                              "text-red-600": error !== null,
                            }}
                          >
                            <Tooltip>
                              <TooltipTrigger as="div">
                                <div class="flex items-center gap-2">
                                  <TbInfoCircle />
                                  <div class="w-[128px] text-sm">
                                    {" "}
                                    {t.toUTCString()}{" "}
                                  </div>
                                </div>
                              </TooltipTrigger>

                              <TooltipContent>
                                <p>Start: {t.toLocaleString()} (Local)</p>
                                <p>
                                  Duration: {Number(durationMillis) / 1000}s
                                </p>
                                <p>Error: {error ?? "none"}</p>
                              </TooltipContent>
                            </Tooltip>
                          </div>
                        );
                      };

                      return (
                        <TableRow>
                          {/*
                          <TableCell>{proxy().config.id}</TableCell>
                          */}

                          <TableCell>{proxy().job?.name}</TableCell>

                          <TableCell>
                            <form.Field
                              name={`jobs[${i}].config.schedule`}
                              validators={isValidCronSpec()}
                            >
                              {(field: () => FieldApiT<string | undefined>) => {
                                return (
                                  <>
                                    <TextField>
                                      <TextFieldInput
                                        type="text"
                                        value={field().state.value}
                                        onBlur={field().handleBlur}
                                        autocomplete="off"
                                        onChange={(e: Event) => {
                                          field().handleChange(
                                            (e.target as HTMLInputElement)
                                              .value,
                                          );
                                        }}
                                      />
                                    </TextField>

                                    <FieldInfo field={field()} />
                                  </>
                                );
                              }}
                            </form.Field>
                          </TableCell>

                          <TableCell>{next()}</TableCell>

                          <TableCell>{latest()}</TableCell>

                          <TableCell>
                            <form.Field name={`jobs[${i}].config.disabled`}>
                              {(field: () => FieldApiT<boolean>) => {
                                const enabled = () =>
                                  !(field().state.value ?? false);
                                return (
                                  <div class="flex items-center justify-center">
                                    <Checkbox
                                      checked={enabled()}
                                      onBlur={field().handleBlur}
                                      onChange={(enabled: boolean) =>
                                        field().handleChange(!enabled)
                                      }
                                    />
                                  </div>
                                );
                              }}
                            </form.Field>
                          </TableCell>

                          <TableCell>
                            <div class="flex h-full items-center">
                              <IconButton
                                tooltip="Run now"
                                onClick={() => {
                                  const id = proxy().job?.id;
                                  if (id) {
                                    (async () => {
                                      const result = await runJob({ id });
                                      console.info(
                                        "execution result: ",
                                        result.error,
                                      );

                                      props.refetchJobs();
                                    })().catch(console.error);
                                  }
                                }}
                              >
                                <TbPlayerPlay size={20} />
                              </IconButton>
                            </div>
                          </TableCell>
                        </TableRow>
                      );
                    }}
                  </Index>
                )}
              </form.Field>
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <div class="flex justify-end pt-4">
        <form.Subscribe
          selector={(state) => ({
            canSubmit: state.canSubmit,
            isSubmitting: state.isSubmitting,
          })}
        >
          {(state) => {
            return (
              <Button
                type="submit"
                disabled={!state().canSubmit}
                variant="default"
              >
                {state().isSubmitting ? "..." : "Submit"}
              </Button>
            );
          }}
        </form.Subscribe>
      </div>
    </form>
  );
}

const listJobsKey = ["admin", "jobs"];

export function JobSettings(props: {
  markDirty: () => void;
  postSubmit: () => void;
}) {
  const queryClient = useQueryClient();
  const config = createConfigQuery();
  const jobList = useQuery(() => ({
    queryKey: listJobsKey,
    queryFn: listJobs,
  }));

  return (
    <Switch fallback="Loading...">
      <Match when={jobList.isError}>{jobList.error?.toString()}</Match>
      <Match when={config.error}>{JSON.stringify(config.error)}</Match>

      <Match when={jobList.isSuccess && config.data?.config}>
        <JobSettingsImpl
          {...props}
          config={config.data!.config!}
          jobs={jobList.data?.jobs ?? []}
          refetchJobs={() => {
            queryClient.invalidateQueries({
              queryKey: listJobsKey,
            });
          }}
        />
      </Match>
    </Switch>
  );
}
