import { createResource, For, Switch, Match } from "solid-js";
import { createForm } from "@tanstack/solid-form";
import { TbPlayerPlay, TbHistory } from "solid-icons/tb";

import { Button } from "@/components/ui/button";
import { IconButton } from "@/components/IconButton";
import { Checkbox } from "@/components/ui/checkbox";
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

import { type FieldApiT, notEmptyValidator } from "@/components/FormFields";
import { Config, JobsConfig, SystemJob } from "@proto/config";
import { createConfigQuery, setConfig } from "@/lib/config";
import { listJobs } from "@/lib/jobs";
import type { Job } from "@bindings/Job";

type CronJobProxy = {
  /// Set to false if the loaded config contained the job.
  default: boolean;
  initialConfig: SystemJob;
  config: SystemJob;
  job?: Job;
};

type FormProxy = {
  jobs: CronJobProxy[];
};

function equal(a: SystemJob, b: SystemJob): boolean {
  return (
    a.disabled === b.disabled && a.schedule === b.schedule && a.id === b.id
  );
}

function buildFormProxy(
  config: JobsConfig | undefined,
  jobs: Job[],
): FormProxy {
  const result = new Map<number, CronJobProxy>();
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
    };
    const entry: CronJobProxy = result.get(job.id) ?? {
      default: true,
      initialConfig: d,
      config: { ...d },
    };
    entry.job = job;
    result.set(job.id, entry);
  }

  return {
    jobs: [...result.values()],
  };
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

export function JobSettingsImpl(props: {
  markDirty: () => void;
  postSubmit: () => void;
  config: Config;
  jobs: Job[];
  refetchJobs: () => void;
}) {
  const form = createForm(() => ({
    defaultValues: buildFormProxy(props.config.jobs, props.jobs),
    onSubmit: async ({ value }: { value: FormProxy }) => {
      const newConfig = {
        ...props.config,
        cron: extractConfig(value),
      };

      await setConfig(newConfig);
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
      onSubmit={(e) => {
        e.preventDefault();
        e.stopPropagation();
        form.handleSubmit();
      }}
    >
      <Table>
        <TableHeader>
          <TableHead>Id</TableHead>
          <TableHead>Name</TableHead>
          <TableHead>Schedule</TableHead>
          <TableHead>Next</TableHead>
          <TableHead>Last</TableHead>
          <TableHead>Enabled</TableHead>
          <TableHead>Action</TableHead>
        </TableHeader>

        <TableBody>
          <For each={props.jobs}>
            {(job: Job, index: () => number) => {
              const next = () => {
                const timestamp = job.next;
                if (!timestamp) return null;

                const t = new Date(Number(timestamp) * 1000);

                return (
                  <Tooltip>
                    <TooltipTrigger as="div">
                      <div class="w-[128px]">{t.toUTCString()}</div>
                    </TooltipTrigger>

                    <TooltipContent>
                      {t.toLocaleString()} (Local)
                    </TooltipContent>
                  </Tooltip>
                );
              };

              const latest = () => {
                const latest = job.latest;
                if (!latest) return null;

                const [timestamp, error] = latest;
                const t = new Date(Number(timestamp) * 1000);

                return (
                  <div>
                    {t.toUTCString()}
                    {error}
                  </div>
                );
              };

              return (
                <TableRow>
                  <TableCell>{job.id}</TableCell>

                  <TableCell>{job.name}</TableCell>

                  <TableCell>
                    <form.Field
                      name={`jobs[${index()}].config.schedule`}
                      validators={notEmptyValidator()}
                    >
                      {(field: () => FieldApiT<string | undefined>) => {
                        return (
                          <TextField>
                            <TextFieldInput
                              type="text"
                              value={field().state.value}
                              onBlur={field().handleBlur}
                              autocomplete="off"
                              onKeyUp={(e: Event) => {
                                field().handleChange(
                                  (e.target as HTMLInputElement).value,
                                );
                              }}
                            />
                          </TextField>
                        );
                      }}
                    </form.Field>
                  </TableCell>

                  <TableCell>{next()}</TableCell>

                  <TableCell>{latest()}</TableCell>

                  <TableCell>
                    <div class="flex items-center justify-center">
                      <Checkbox checked={job.enabled} />
                    </div>
                  </TableCell>

                  <TableCell>
                    <div class="flex h-full items-center">
                      <IconButton
                        onClick={() => {
                          props.refetchJobs();
                        }}
                      >
                        <TbPlayerPlay size={20} />
                      </IconButton>

                      <IconButton onClick={() => {}}>
                        <TbHistory size={20} />
                      </IconButton>
                    </div>
                  </TableCell>
                </TableRow>
              );
            }}
          </For>
        </TableBody>
      </Table>

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

export function JobSettings(props: {
  markDirty: () => void;
  postSubmit: () => void;
}) {
  const config = createConfigQuery();
  const [jobList, { refetch }] = createResource(listJobs);

  return (
    <Switch fallback="Loading...">
      <Match when={jobList.error}>{jobList.error}</Match>
      <Match when={config.error}>{JSON.stringify(config.error)}</Match>

      <Match when={jobList() && config.data?.config}>
        <JobSettingsImpl
          {...props}
          config={config.data!.config!}
          jobs={jobList()?.jobs ?? []}
          refetchJobs={refetch}
        />
      </Match>
    </Switch>
  );
}
