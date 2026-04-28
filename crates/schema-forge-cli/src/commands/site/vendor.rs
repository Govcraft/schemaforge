//! Vendored static assets for the site generator.
//!
//! These constants are baked into the binary and emitted verbatim (with a
//! [`Marker`][super::super::codegen::marker::Marker] header prepended by the
//! write layer) into the output project.
//!
//! **shadcn/ui files** are vendored under the MIT license from
//! <https://github.com/shadcn-ui/ui>. The only modification across the set is
//! a single import path rewrite in `form.tsx` (`@/registry/new-york-v4/ui/label`
//! → `@/components/ui/label`) so the file works outside the upstream monorepo.
//! Refresh by re-fetching from
//! `https://raw.githubusercontent.com/shadcn-ui/ui/main/apps/v4/registry/new-york-v4/ui/<file>.tsx`.

pub const SHADCN_UTILS_TS: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
"#;

pub const SHADCN_BUTTON: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { Slot } from "radix-ui"

import { cn } from "@/lib/utils"

const buttonVariants = cva(
  "inline-flex shrink-0 items-center justify-center gap-2 rounded-md text-sm font-medium whitespace-nowrap transition-all outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 aria-invalid:border-destructive aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40 [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-4",
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground hover:bg-primary/90",
        destructive:
          "bg-destructive text-white hover:bg-destructive/90 focus-visible:ring-destructive/20 dark:bg-destructive/60 dark:focus-visible:ring-destructive/40",
        outline:
          "border bg-background shadow-xs hover:bg-accent hover:text-accent-foreground dark:border-input dark:bg-input/30 dark:hover:bg-input/50",
        secondary:
          "bg-secondary text-secondary-foreground hover:bg-secondary/80",
        ghost:
          "hover:bg-accent hover:text-accent-foreground dark:hover:bg-accent/50",
        link: "text-primary underline-offset-4 hover:underline",
      },
      size: {
        default: "h-9 px-4 py-2 has-[>svg]:px-3",
        xs: "h-6 gap-1 rounded-md px-2 text-xs has-[>svg]:px-1.5 [&_svg:not([class*='size-'])]:size-3",
        sm: "h-8 gap-1.5 rounded-md px-3 has-[>svg]:px-2.5",
        lg: "h-10 rounded-md px-6 has-[>svg]:px-4",
        icon: "size-9",
        "icon-xs": "size-6 rounded-md [&_svg:not([class*='size-'])]:size-3",
        "icon-sm": "size-8",
        "icon-lg": "size-10",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  }
)

function Button({
  className,
  variant = "default",
  size = "default",
  asChild = false,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean
  }) {
  const Comp = asChild ? Slot.Root : "button"

  return (
    <Comp
      data-slot="button"
      data-variant={variant}
      data-size={size}
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  )
}

export { Button, buttonVariants }
"#;

pub const SHADCN_INPUT: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
import * as React from "react"

import { cn } from "@/lib/utils"

function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "h-9 w-full min-w-0 rounded-md border border-input bg-transparent px-3 py-1 text-base shadow-xs transition-[color,box-shadow] outline-none selection:bg-primary selection:text-primary-foreground file:inline-flex file:h-7 file:border-0 file:bg-transparent file:text-sm file:font-medium file:text-foreground placeholder:text-muted-foreground disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50 md:text-sm dark:bg-input/30",
        "focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50",
        "aria-invalid:border-destructive aria-invalid:ring-destructive/20 dark:aria-invalid:ring-destructive/40",
        className
      )}
      {...props}
    />
  )
}

export { Input }
"#;

pub const SHADCN_LABEL: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
"use client"

import * as React from "react"
import { Label as LabelPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"

function Label({
  className,
  ...props
}: React.ComponentProps<typeof LabelPrimitive.Root>) {
  return (
    <LabelPrimitive.Root
      data-slot="label"
      className={cn(
        "flex items-center gap-2 text-sm leading-none font-medium select-none group-data-[disabled=true]:pointer-events-none group-data-[disabled=true]:opacity-50 peer-disabled:cursor-not-allowed peer-disabled:opacity-50",
        className
      )}
      {...props}
    />
  )
}

export { Label }
"#;

pub const SHADCN_CARD: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
import * as React from "react"

import { cn } from "@/lib/utils"

function Card({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card"
      className={cn(
        "flex flex-col gap-6 rounded-xl border bg-card py-6 text-card-foreground shadow-sm",
        className
      )}
      {...props}
    />
  )
}

function CardHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-header"
      className={cn(
        "@container/card-header grid auto-rows-min grid-rows-[auto_auto] items-start gap-2 px-6 has-data-[slot=card-action]:grid-cols-[1fr_auto] [.border-b]:pb-6",
        className
      )}
      {...props}
    />
  )
}

function CardTitle({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-title"
      className={cn("leading-none font-semibold", className)}
      {...props}
    />
  )
}

function CardDescription({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-description"
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

function CardAction({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-action"
      className={cn(
        "col-start-2 row-span-2 row-start-1 self-start justify-self-end",
        className
      )}
      {...props}
    />
  )
}

function CardContent({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-content"
      className={cn("px-6", className)}
      {...props}
    />
  )
}

function CardFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-footer"
      className={cn("flex items-center px-6 [.border-t]:pt-6", className)}
      {...props}
    />
  )
}

export {
  Card,
  CardHeader,
  CardFooter,
  CardTitle,
  CardAction,
  CardDescription,
  CardContent,
}
"#;

pub const SHADCN_FORM: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
// Modified: the upstream `@/registry/new-york-v4/ui/label` import is rewritten
// to `@/components/ui/label` so the file resolves inside a standalone project.
"use client"

import * as React from "react"
import type { Label as LabelPrimitive } from "radix-ui"
import { Slot } from "radix-ui"
import {
  Controller,
  FormProvider,
  useFormContext,
  useFormState,
  type ControllerProps,
  type FieldPath,
  type FieldValues,
} from "react-hook-form"

import { cn } from "@/lib/utils"
import { Label } from "@/components/ui/label"

const Form = FormProvider

type FormFieldContextValue<
  TFieldValues extends FieldValues = FieldValues,
  TName extends FieldPath<TFieldValues> = FieldPath<TFieldValues>,
> = {
  name: TName
}

const FormFieldContext = React.createContext<FormFieldContextValue>(
  {} as FormFieldContextValue
)

const FormField = <
  TFieldValues extends FieldValues = FieldValues,
  TName extends FieldPath<TFieldValues> = FieldPath<TFieldValues>,
>({
  ...props
}: ControllerProps<TFieldValues, TName>) => {
  return (
    <FormFieldContext.Provider value={{ name: props.name }}>
      <Controller {...props} />
    </FormFieldContext.Provider>
  )
}

const useFormField = () => {
  const fieldContext = React.useContext(FormFieldContext)
  const itemContext = React.useContext(FormItemContext)
  const { getFieldState } = useFormContext()
  const formState = useFormState({ name: fieldContext.name })
  const fieldState = getFieldState(fieldContext.name, formState)

  if (!fieldContext) {
    throw new Error("useFormField should be used within <FormField>")
  }

  const { id } = itemContext

  return {
    id,
    name: fieldContext.name,
    formItemId: `${id}-form-item`,
    formDescriptionId: `${id}-form-item-description`,
    formMessageId: `${id}-form-item-message`,
    ...fieldState,
  }
}

type FormItemContextValue = {
  id: string
}

const FormItemContext = React.createContext<FormItemContextValue>(
  {} as FormItemContextValue
)

function FormItem({ className, ...props }: React.ComponentProps<"div">) {
  const id = React.useId()

  return (
    <FormItemContext.Provider value={{ id }}>
      <div
        data-slot="form-item"
        className={cn("grid gap-2", className)}
        {...props}
      />
    </FormItemContext.Provider>
  )
}

function FormLabel({
  className,
  ...props
}: React.ComponentProps<typeof LabelPrimitive.Root>) {
  const { error, formItemId } = useFormField()

  return (
    <Label
      data-slot="form-label"
      data-error={!!error}
      className={cn("data-[error=true]:text-destructive", className)}
      htmlFor={formItemId}
      {...props}
    />
  )
}

function FormControl({ ...props }: React.ComponentProps<typeof Slot.Root>) {
  const { error, formItemId, formDescriptionId, formMessageId } = useFormField()

  return (
    <Slot.Root
      data-slot="form-control"
      id={formItemId}
      aria-describedby={
        !error
          ? `${formDescriptionId}`
          : `${formDescriptionId} ${formMessageId}`
      }
      aria-invalid={!!error}
      {...props}
    />
  )
}

function FormDescription({ className, ...props }: React.ComponentProps<"p">) {
  const { formDescriptionId } = useFormField()

  return (
    <p
      data-slot="form-description"
      id={formDescriptionId}
      className={cn("text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

function FormMessage({ className, ...props }: React.ComponentProps<"p">) {
  const { error, formMessageId } = useFormField()
  const body = error ? String(error?.message ?? "") : props.children

  if (!body) {
    return null
  }

  return (
    <p
      data-slot="form-message"
      id={formMessageId}
      className={cn("text-sm text-destructive", className)}
      {...props}
    >
      {body}
    </p>
  )
}

export {
  useFormField,
  Form,
  FormItem,
  FormLabel,
  FormControl,
  FormDescription,
  FormMessage,
  FormField,
}
"#;

pub const SHADCN_TABLE: &str = r#"// Vendored from shadcn/ui — MIT License — https://github.com/shadcn-ui/ui
"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

function Table({ className, ...props }: React.ComponentProps<"table">) {
  return (
    <div
      data-slot="table-container"
      className="relative w-full overflow-x-auto"
    >
      <table
        data-slot="table"
        className={cn("w-full caption-bottom text-sm", className)}
        {...props}
      />
    </div>
  )
}

function TableHeader({ className, ...props }: React.ComponentProps<"thead">) {
  return (
    <thead
      data-slot="table-header"
      className={cn("[&_tr]:border-b", className)}
      {...props}
    />
  )
}

function TableBody({ className, ...props }: React.ComponentProps<"tbody">) {
  return (
    <tbody
      data-slot="table-body"
      className={cn("[&_tr:last-child]:border-0", className)}
      {...props}
    />
  )
}

function TableFooter({ className, ...props }: React.ComponentProps<"tfoot">) {
  return (
    <tfoot
      data-slot="table-footer"
      className={cn(
        "border-t bg-muted/50 font-medium [&>tr]:last:border-b-0",
        className
      )}
      {...props}
    />
  )
}

function TableRow({ className, ...props }: React.ComponentProps<"tr">) {
  return (
    <tr
      data-slot="table-row"
      className={cn(
        "border-b transition-colors hover:bg-muted/50 has-aria-expanded:bg-muted/50 data-[state=selected]:bg-muted",
        className
      )}
      {...props}
    />
  )
}

function TableHead({ className, ...props }: React.ComponentProps<"th">) {
  return (
    <th
      data-slot="table-head"
      className={cn(
        "h-10 px-2 text-left align-middle font-medium whitespace-nowrap text-foreground [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]",
        className
      )}
      {...props}
    />
  )
}

function TableCell({ className, ...props }: React.ComponentProps<"td">) {
  return (
    <td
      data-slot="table-cell"
      className={cn(
        "p-2 align-middle whitespace-nowrap [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]",
        className
      )}
      {...props}
    />
  )
}

function TableCaption({
  className,
  ...props
}: React.ComponentProps<"caption">) {
  return (
    <caption
      data-slot="table-caption"
      className={cn("mt-4 text-sm text-muted-foreground", className)}
      {...props}
    />
  )
}

export {
  Table,
  TableHeader,
  TableBody,
  TableFooter,
  TableHead,
  TableRow,
  TableCell,
  TableCaption,
}
"#;

// ---------------------------------------------------------------------------
// Schema-forge components (not from shadcn)
// ---------------------------------------------------------------------------

/// A generic relation picker: renders a `<select>` whose options are
/// fetched from the backend for a given schema target and display field.
/// Value is the selected entity id (or `undefined`).
pub const RELATION_SELECT: &str = r#"// Generated by schema-forge — edit freely.
import { forwardRef } from "react"
import { useQuery } from "@tanstack/react-query"
import { rawEntityList } from "@/generated/api-client"

export type RelationSelectProps = {
  target: string
  displayField?: string
  value?: string
  onChange: (value: string) => void
  onBlur?: () => void
  name?: string
  placeholder?: string
  disabled?: boolean
}

type RelationRow = { id: string; [key: string]: unknown }

/// Pick the best display label for a fetched relation row:
///   1. `@display("field")` if set and present
///   2. first string-valued field we encounter
///   3. the entity id itself
function labelFor(row: RelationRow, displayField?: string): string {
  if (displayField && typeof row[displayField] === "string") {
    return `${row[displayField] as string}`
  }
  for (const [k, v] of Object.entries(row)) {
    if (k === "id") continue
    if (typeof v === "string" && v.length > 0) return v
  }
  return row.id
}

export const RelationSelect = forwardRef<HTMLSelectElement, RelationSelectProps>(
  function RelationSelect(
    { target, displayField, value, onChange, onBlur, name, placeholder, disabled },
    ref,
  ) {
    const { data, isLoading, error } = useQuery<RelationRow[]>({
      queryKey: ["relation-options", target],
      queryFn: () => rawEntityList(target) as Promise<RelationRow[]>,
      staleTime: 30_000,
    })

    return (
      <select
        ref={ref}
        name={name}
        value={value ?? ""}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onBlur}
        disabled={disabled || isLoading || Boolean(error)}
        className="h-9 w-full rounded-md border border-input bg-transparent px-3 text-sm"
      >
        <option value="">{placeholder ?? `— no ${target} selected —`}</option>
        {error ? (
          <option value="" disabled>
            Failed to load {target}
          </option>
        ) : null}
        {(data ?? []).map((row) => (
          <option key={row.id} value={row.id}>
            {labelFor(row, displayField)}
          </option>
        ))}
      </select>
    )
  },
)
"#;

// ---------------------------------------------------------------------------
// Project-level static files (not from shadcn)
// ---------------------------------------------------------------------------

pub const TSCONFIG_JSON: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "useDefineForClassFields": true,
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "module": "ESNext",
    "skipLibCheck": true,
    "moduleResolution": "bundler",
    "allowImportingTsExtensions": true,
    "resolveJsonModule": true,
    "isolatedModules": true,
    "moduleDetection": "force",
    "noEmit": true,
    "jsx": "react-jsx",
    "strict": true,
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noFallthroughCasesInSwitch": true,
    "types": ["vite/client"],
    "baseUrl": ".",
    "paths": {
      "@/*": ["./src/*"]
    }
  },
  "include": ["src"],
  "references": [{ "path": "./tsconfig.node.json" }]
}
"#;

pub const TSCONFIG_NODE_JSON: &str = r#"{
  "compilerOptions": {
    "composite": true,
    "skipLibCheck": true,
    "module": "ESNext",
    "moduleResolution": "bundler",
    "allowSyntheticDefaultImports": true,
    "strict": true
  },
  "include": ["vite.config.ts"]
}
"#;

pub const GITIGNORE: &str = r#"node_modules
dist
.vite
*.log
*.tsbuildinfo
.DS_Store
"#;

/// Govcraft brand mark, white fill — used on the inked sidebar rail and
/// the dark login left panel. Vendored from the Govcraft DS (paths only,
/// no rasterized stroke). Vite's `public/` dir serves these at the URL
/// root, so the React templates can `<img src="/logo-mark-white.svg" />`
/// without bundler involvement.
pub const LOGO_MARK_WHITE_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<svg version="1.1" id="svg1" width="886.4729" height="720.97095" viewBox="0 0 886.47289 720.97095" xmlns="http://www.w3.org/2000/svg" xmlns:svg="http://www.w3.org/2000/svg">
  <defs id="defs1"></defs>
  <g id="layer-MC0" transform="translate(9.3134156e-4,-75.917999)">
    <path id="path1" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.4512,284.1136)"></path>
    <path id="path2" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.45107,400.17613)"></path>
    <path id="path3" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,474.93093,400.17613)"></path>
    <path id="path4" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,679.4108,400.17627)"></path>
    <path id="path5" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.68787,515.8356)"></path>
    <path id="path6" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.68787,631.89813)"></path>
    <path id="path7" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,475.24653,631.89827)"></path>
    <path id="path8" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,543.4328,631.89827)"></path>
    <path id="path9" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,679.80533,631.89827)"></path>
    <path id="path10" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,679.80533,515.83573)"></path>
    <path id="path11" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#ffffff;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,543.19613,284.1136)"></path>
  </g>
</svg>
"##;

/// Govcraft brand mark, ink fill — used as the favicon and on light surfaces.
pub const LOGO_MARK_INK_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<svg version="1.1" id="svg1" width="886.4729" height="720.97095" viewBox="0 0 886.47289 720.97095" xmlns="http://www.w3.org/2000/svg" xmlns:svg="http://www.w3.org/2000/svg">
  <defs id="defs1"></defs>
  <g id="layer-MC0" transform="translate(9.3134156e-4,-75.917999)">
    <path id="path1" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.4512,284.1136)"></path>
    <path id="path2" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.45107,400.17613)"></path>
    <path id="path3" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,474.93093,400.17613)"></path>
    <path id="path4" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,679.4108,400.17627)"></path>
    <path id="path5" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.68787,515.8356)"></path>
    <path id="path6" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,270.68787,631.89813)"></path>
    <path id="path7" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,475.24653,631.89827)"></path>
    <path id="path8" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,543.4328,631.89827)"></path>
    <path id="path9" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,679.80533,631.89827)"></path>
    <path id="path10" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,679.80533,515.83573)"></path>
    <path id="path11" d="M 0,0 H -46.026 V 78.342 H 0 Z" style="fill:#000000;fill-opacity:1;fill-rule:nonzero;stroke:none" transform="matrix(1.3333333,0,0,-1.3333333,543.19613,284.1136)"></path>
  </g>
</svg>
"##;

pub const INDEX_CSS: &str = r#"@import "tailwindcss";

/* =========================================================
   SchemaForge generated-site baseline — Govcraft Design System
   Two type families (IBM Plex Sans + Mono), one signal accent
   (Signal Orange), paper ground + ink in light, console in dark.
   shadcn primitive tokens are rebound onto these Govcraft tokens
   so Button / Input / Card render in-brand without changes.
   ========================================================= */

/* ---- Govcraft palette + DS tokens (light is default) ---- */
:root {
  --gc-ink:        #0A0A0A;
  --gc-ink-2:      #1F1F1F;
  --gc-graphite:   #3A3A3A;
  --gc-steel:      #6B6B6B;
  --gc-mist:       #9A9A9A;
  --gc-hairline:   #E4E1DA;
  --gc-rule:       #CFCAC0;
  --gc-paper:      #F5F2EC;
  --gc-paper-2:    #EEEAE1;
  --gc-paper-3:    #E7E2D6;
  --gc-white:      #FFFFFF;
  --gc-signal-50:  #FFF3EB;
  --gc-signal-400: #F97316;
  --gc-signal-500: #D9590B;
  --gc-signal-600: #A8430A;
  --gc-navy-500:   #1F3350;
  --gc-ok-400:     #2E7D3A;
  --gc-warn-400:   #B7791F;
  --gc-err-400:    #B4321A;
  --gc-err-600:    #7E1F0E;

  --font-sans:
    "IBM Plex Sans", -apple-system, BlinkMacSystemFont,
    "Segoe UI", system-ui, sans-serif;
  --font-mono:
    "IBM Plex Mono", ui-monospace, SFMono-Regular,
    Menlo, Consolas, monospace;

  --ease-out:    cubic-bezier(0.2, 0, 0, 1);
  --ease-in-out: cubic-bezier(0.4, 0, 0.2, 1);
  --dur-fast:    120ms;
  --dur-base:    180ms;

  --shadow-2: 0 1px 2px 0 rgba(10,10,10,0.08), 0 0 0 1px rgba(10,10,10,0.04);
  --shadow-3: 0 4px 12px -2px rgba(10,10,10,0.12), 0 0 0 1px rgba(10,10,10,0.06);
  --shadow-4: 0 12px 32px -8px rgba(10,10,10,0.18), 0 0 0 1px rgba(10,10,10,0.08);

  /* App-surface tokens (the design's --app-* set) */
  --app-bg:           var(--gc-paper);
  --app-bg-2:         var(--gc-paper-2);
  --app-bg-raised:    var(--gc-white);
  --app-fg-1:         var(--gc-ink);
  --app-fg-2:         var(--gc-graphite);
  --app-fg-3:         var(--gc-steel);
  --app-fg-4:         var(--gc-mist);
  --app-border:       var(--gc-hairline);
  --app-border-2:     var(--gc-rule);
  --app-rail:         var(--gc-ink);
  --app-rail-fg:      var(--gc-paper);
  --app-rail-fg-2:    #C9C7C1;
  --app-rail-fg-3:    #6B6B6B;
  --app-rail-line:    #2A2A2A;
  --app-rail-active:  #1F1F1F;
  --app-row-hover:    rgba(10,10,10,0.035);
  --app-row-selected: rgba(249,115,22,0.08);
  --app-table-head:   var(--gc-paper-2);
  --app-grid:         rgba(10,10,10,0.04);

  --accent:           var(--gc-signal-400);
  --accent-hover:     var(--gc-signal-500);
  --accent-fg:        var(--gc-white);
  --accent-bg-soft:   var(--gc-signal-50);
  --link:             var(--gc-navy-500);
}

[data-theme="dark"] {
  --app-bg:           #0B0B0B;
  --app-bg-2:         #141414;
  --app-bg-raised:    #181818;
  --app-fg-1:         #F0EDE5;
  --app-fg-2:         #B8B5AD;
  --app-fg-3:         #8A8780;
  --app-fg-4:         #5C5A55;
  --app-border:       #262626;
  --app-border-2:     #333333;
  --app-rail:         #050505;
  --app-rail-fg:      #F0EDE5;
  --app-rail-fg-2:    #B8B5AD;
  --app-rail-fg-3:    #6A6760;
  --app-rail-line:    #1A1A1A;
  --app-rail-active:  #161616;
  --app-row-hover:    rgba(255,255,255,0.03);
  --app-row-selected: rgba(249,115,22,0.12);
  --app-table-head:   #101010;
  --app-grid:         rgba(255,255,255,0.04);
  --link:             #91A8C8;
}

/* ---- Bind shadcn v4 tokens onto Govcraft DS so primitives render in-brand ---- */
@theme inline {
  --color-background:        var(--app-bg);
  --color-foreground:        var(--app-fg-1);
  --color-card:              var(--app-bg-raised);
  --color-card-foreground:   var(--app-fg-1);
  --color-popover:           var(--app-bg-raised);
  --color-popover-foreground:var(--app-fg-1);
  --color-primary:           var(--gc-ink);
  --color-primary-foreground:var(--gc-paper);
  --color-secondary:         var(--app-bg-2);
  --color-secondary-foreground: var(--app-fg-1);
  --color-muted:             var(--app-bg-2);
  --color-muted-foreground:  var(--app-fg-3);
  --color-accent:            var(--accent);
  --color-accent-foreground: var(--accent-fg);
  --color-destructive:       var(--gc-err-400);
  --color-destructive-foreground: var(--gc-white);
  --color-border:            var(--app-border);
  --color-input:             var(--app-border-2);
  --color-ring:              var(--app-fg-1);
  --radius:                  2px;

  --font-sans: var(--font-sans);
  --font-mono: var(--font-mono);
}
[data-theme="dark"] {
  --color-primary:           var(--gc-paper);
  --color-primary-foreground:var(--gc-ink);
}

/* ---- Document defaults ---- */
*, *::before, *::after { box-sizing: border-box; }
html, body, #root { height: 100%; }
body {
  margin: 0;
  background: var(--app-bg);
  color: var(--app-fg-1);
  font-family: var(--font-sans);
  font-size: 14px;
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}
::selection { background: var(--gc-ink); color: var(--gc-paper); }

/* ---- Reusable atoms ---- */
.mono { font-family: var(--font-mono); }
.eyebrow {
  font-family: var(--font-mono);
  font-weight: 500;
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--app-fg-3);
}
.micro { font-size: 11px; color: var(--app-fg-3); }
.hairline { border-top: 1px solid var(--app-border); }
.kbd, kbd {
  display: inline-flex; align-items: center; justify-content: center;
  min-width: 18px; height: 18px;
  padding: 0 4px;
  font-family: var(--font-mono); font-size: 10.5px; font-weight: 500;
  color: var(--app-fg-2);
  background: var(--app-bg-2);
  border: 1px solid var(--app-border);
  border-bottom-width: 2px;
  border-radius: 3px;
  line-height: 1;
}

/* ---- Buttons (used directly via className for raw <button> elements) ---- */
.btn {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 6px 12px;
  font-family: inherit;
  font-size: 13px; font-weight: 500;
  border-radius: 2px;
  border: 1px solid var(--app-border-2);
  background: var(--app-bg-raised);
  color: var(--app-fg-1);
  cursor: pointer;
  white-space: nowrap;
  transition: background var(--dur-fast) var(--ease-out),
              border-color var(--dur-fast) var(--ease-out),
              transform var(--dur-fast) var(--ease-out);
}
.btn:hover { background: var(--app-bg-2); }
.btn:active { transform: translateY(1px); }
.btn:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px var(--app-bg), 0 0 0 4px var(--app-fg-1);
}
.btn:disabled { opacity: 0.55; cursor: not-allowed; }
.btn-sm { padding: 4px 8px; font-size: 12px; }
.btn-primary {
  background: var(--gc-ink); color: var(--gc-paper);
  border-color: var(--gc-ink);
}
[data-theme="dark"] .btn-primary {
  background: var(--gc-paper); color: var(--gc-ink); border-color: var(--gc-paper);
}
.btn-primary:hover { background: var(--gc-ink-2); }
[data-theme="dark"] .btn-primary:hover { background: #E5E2D9; }
.btn-accent {
  background: var(--accent); color: var(--accent-fg);
  border-color: var(--accent);
}
.btn-accent:hover {
  background: var(--accent-hover);
  border-color: var(--accent-hover);
}
.btn-danger {
  background: var(--gc-err-400); color: #fff; border-color: var(--gc-err-400);
}
.btn-danger:hover { background: var(--gc-err-600); border-color: var(--gc-err-600); }
.btn-ghost { background: transparent; border-color: transparent; }
.btn-ghost:hover { background: var(--app-row-hover); border-color: var(--app-border); }

/* ---- Inputs (raw `<input className="input">` etc.) ---- */
.input, .select, .textarea {
  width: 100%;
  padding: 6px 10px;
  font-family: inherit;
  font-size: 13px;
  background: var(--app-bg-raised);
  color: var(--app-fg-1);
  border: 1px solid var(--app-border-2);
  border-radius: 2px;
  transition: border-color var(--dur-fast) var(--ease-out),
              box-shadow var(--dur-fast) var(--ease-out);
}
.input:focus, .select:focus, .textarea:focus {
  outline: none;
  border-color: var(--app-fg-1);
  box-shadow: 0 0 0 1px var(--app-fg-1);
}
.input.invalid, .select.invalid, .textarea.invalid {
  border-color: var(--gc-err-400);
  box-shadow: 0 0 0 1px var(--gc-err-400);
}
.input::placeholder { color: var(--app-fg-4); }

/* ---- Status chips ---- */
.chip {
  display: inline-flex; align-items: center; gap: 4px;
  padding: 1px 8px;
  font-family: var(--font-mono);
  font-size: 10.5px; font-weight: 500;
  letter-spacing: 0.06em; text-transform: uppercase;
  border-radius: 999px;
  border: 1px solid var(--app-border-2);
  color: var(--app-fg-2);
  background: transparent;
  white-space: nowrap;
}
.chip-ok    { color: var(--gc-ok-400);   border-color: var(--gc-ok-400); }
.chip-warn  { color: var(--gc-warn-400); border-color: var(--gc-warn-400); }
.chip-err   { color: var(--gc-err-400);  border-color: var(--gc-err-400); }
.chip-info  { color: var(--gc-navy-500); border-color: var(--gc-navy-500); }
.chip-accent{ color: var(--accent);      border-color: var(--accent); }
[data-theme="dark"] .chip-info { color: #6B89B8; border-color: #4A638A; }

/* ---- App shell ---- */
.shell {
  display: grid;
  grid-template-columns: 232px 1fr;
  grid-template-rows: 56px 1fr;
  grid-template-areas: "rail topbar" "rail main";
  height: 100vh;
}
.shell-rail {
  grid-area: rail;
  background: var(--app-rail);
  color: var(--app-rail-fg);
  display: flex; flex-direction: column;
  border-right: 1px solid var(--app-rail-line);
  min-width: 0;
}
.shell-topbar {
  grid-area: topbar;
  display: flex; align-items: center; gap: 16px;
  padding: 0 16px;
  border-bottom: 1px solid var(--app-border);
  background: var(--app-bg);
}
.shell-main {
  grid-area: main; overflow: auto;
  background: var(--app-bg);
}

/* Sidebar */
.rail-head {
  display: flex; align-items: center; gap: 10px;
  height: 56px;
  padding: 0 16px;
  border-bottom: 1px solid var(--app-rail-line);
}
.rail-mark { width: 22px; height: 22px; flex: none; }
.rail-mark img, .rail-mark svg { width: 100%; height: 100%; display: block; }
.rail-title { font-size: 13px; font-weight: 600; line-height: 1.1; }
.rail-sub {
  font-family: var(--font-mono); font-size: 9.5px;
  letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--app-rail-fg-3); margin-top: 2px;
}
.rail-section { padding: 14px 12px 4px; }
.rail-section-label {
  font-family: var(--font-mono); font-size: 10px; font-weight: 500;
  letter-spacing: 0.14em; text-transform: uppercase;
  color: var(--app-rail-fg-3);
  padding: 0 6px 6px;
  display: flex; align-items: center; justify-content: space-between;
}
.rail-link {
  display: flex; align-items: center; gap: 8px;
  padding: 6px 8px;
  border-radius: 2px;
  font-size: 13px;
  color: var(--app-rail-fg-2);
  cursor: pointer;
  user-select: none;
  text-decoration: none;
}
.rail-link:hover { background: rgba(255,255,255,0.04); color: var(--app-rail-fg); }
.rail-link.active {
  background: var(--app-rail-active);
  color: var(--app-rail-fg);
  font-weight: 500;
  box-shadow: inset 2px 0 0 0 var(--accent);
}
.rail-link .count {
  margin-left: auto;
  font-family: var(--font-mono); font-size: 11px;
  color: var(--app-rail-fg-3);
}
.rail-link.active .count { color: var(--app-rail-fg-2); }
.rail-link .dot {
  width: 6px; height: 6px; border-radius: 1px;
  background: currentColor; opacity: 0.4; flex: none;
}
.rail-foot {
  margin-top: auto;
  padding: 12px 16px;
  border-top: 1px solid var(--app-rail-line);
  font-family: var(--font-mono); font-size: 10px;
  color: var(--app-rail-fg-3);
  letter-spacing: 0.08em;
}
.rail-foot-line { display: flex; justify-content: space-between; gap: 8px; }
.rail-foot-line + .rail-foot-line { margin-top: 4px; }

/* Topbar */
.crumbs { display: flex; align-items: center; gap: 6px; font-size: 13px; color: var(--app-fg-3); flex: 1; min-width: 0; }
.crumbs .sep { color: var(--app-fg-4); }
.crumbs .now { color: var(--app-fg-1); font-weight: 500; }
.crumbs a { color: inherit; text-decoration: none; cursor: pointer; }
.crumbs a:hover { color: var(--app-fg-1); }

.topbar-action {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 5px 10px;
  border: 1px solid var(--app-border);
  border-radius: 2px;
  background: var(--app-bg-2);
  color: var(--app-fg-2);
  font-size: 12px;
  cursor: pointer;
}
.topbar-action:hover { color: var(--app-fg-1); border-color: var(--app-border-2); }

.avatar {
  width: 24px; height: 24px;
  background: var(--gc-navy-500); color: #fff;
  font-family: var(--font-mono); font-size: 11px;
  display: flex; align-items: center; justify-content: center;
  border-radius: 2px;
}

/* ---- Page wrapper ---- */
.page { padding: 24px 32px 64px; max-width: 1480px; }
.page-narrow { padding: 24px 32px 64px; max-width: 920px; }
.page-head {
  display: flex; align-items: flex-end; justify-content: space-between;
  gap: 16px; margin-bottom: 16px;
}
.page-title { font-size: 22px; font-weight: 600; line-height: 1.15; letter-spacing: -0.01em; }
.page-sub { font-size: 13px; color: var(--app-fg-3); margin-top: 2px; }

/* ---- Toolbar ---- */
.toolbar {
  display: flex; align-items: center; gap: 8px;
  padding: 10px 12px;
  background: var(--app-bg);
  border: 1px solid var(--app-border);
  border-bottom: 0;
  border-radius: 2px 2px 0 0;
}
.toolbar .grow { flex: 1; }
.toolbar-input {
  display: flex; align-items: center; gap: 6px;
  padding: 5px 10px; min-width: 240px;
  background: var(--app-bg-2);
  border: 1px solid var(--app-border);
  border-radius: 2px;
  font-size: 13px;
}
.toolbar-input input {
  background: transparent; border: 0; outline: none; flex: 1;
  font-size: 13px; color: var(--app-fg-1);
}
.toolbar-input input::placeholder { color: var(--app-fg-4); }
.toolbar-select {
  padding: 5px 8px; font-size: 12px;
  background: var(--app-bg-2);
  border: 1px solid var(--app-border);
  border-radius: 2px;
  color: var(--app-fg-2);
  font-family: inherit;
}

/* ---- Compact data table ---- */
.table-wrap {
  border: 1px solid var(--app-border);
  border-radius: 0 0 2px 2px;
  background: var(--app-bg-raised);
  overflow: auto;
}
table.tbl { width: 100%; border-collapse: collapse; font-size: 12.5px; }
table.tbl thead th {
  position: sticky; top: 0;
  background: var(--app-table-head);
  text-align: left;
  padding: 8px 12px;
  font-weight: 500;
  font-family: var(--font-mono);
  font-size: 10.5px;
  letter-spacing: 0.1em;
  text-transform: uppercase;
  color: var(--app-fg-3);
  border-bottom: 1px solid var(--app-border);
  white-space: nowrap;
}
table.tbl thead th button {
  font: inherit; color: inherit; background: none; border: 0;
  padding: 0; cursor: pointer; text-transform: inherit; letter-spacing: inherit;
}
table.tbl thead th button:hover { color: var(--app-fg-1); }
table.tbl thead th .sort-arrow { color: var(--accent); margin-left: 4px; }
table.tbl tbody td {
  padding: 7px 12px;
  border-bottom: 1px solid var(--app-border);
  vertical-align: middle;
  white-space: nowrap;
  max-width: 280px;
  overflow: hidden;
  text-overflow: ellipsis;
  color: var(--app-fg-1);
}
table.tbl tbody tr { height: 32px; }
table.tbl tbody tr:hover { background: var(--app-row-hover); }
table.tbl tbody tr.selected { background: var(--app-row-selected); box-shadow: inset 2px 0 0 0 var(--accent); }
table.tbl tbody td.id-cell {
  font-family: var(--font-mono); font-size: 11.5px; color: var(--app-fg-3);
}
table.tbl tbody td.id-cell a { color: var(--link); text-decoration: none; }
table.tbl tbody td.id-cell a:hover { text-decoration: underline; }
table.tbl tbody td .num { font-family: var(--font-mono); }
table.tbl tbody td .muted { color: var(--app-fg-4); font-style: italic; }
table.tbl .actioncell { width: 1px; padding-left: 0; padding-right: 12px; text-align: right; }
table.tbl .row-actions {
  display: flex; gap: 4px; justify-content: flex-end; opacity: 0;
  transition: opacity var(--dur-fast) var(--ease-out);
}
table.tbl tbody tr:hover .row-actions { opacity: 1; }

/* Pager */
.pager {
  display: flex; align-items: center; justify-content: space-between;
  padding: 8px 12px; gap: 8px;
  border: 1px solid var(--app-border);
  border-top: 0;
  border-radius: 0 0 2px 2px;
  background: var(--app-bg);
  font-size: 12px; color: var(--app-fg-3);
}
.pager .group { display: flex; align-items: center; gap: 8px; }

/* ---- Cards ---- */
.card-flat {
  background: var(--app-bg-raised);
  border: 1px solid var(--app-border);
  border-radius: 2px;
}
.card-flat-head {
  padding: 10px 14px;
  border-bottom: 1px solid var(--app-border);
  display: flex; align-items: center; justify-content: space-between;
  gap: 12px;
}
.card-flat-head h3 { font-size: 13px; font-weight: 600; margin: 0; }
.card-flat-body { padding: 14px; }

/* ---- Spec sheet (detail) ---- */
.spec {
  display: grid;
  grid-template-columns: 56px minmax(180px, 240px) 1fr;
  align-items: baseline;
  column-gap: 16px;
}
.spec-row { display: contents; }
.spec-row > * {
  padding: 9px 0;
  border-bottom: 1px solid var(--app-border);
}
.spec-num {
  font-family: var(--font-mono); font-size: 11px;
  color: var(--app-fg-4);
  letter-spacing: 0.05em;
  text-align: right;
}
.spec-key {
  font-family: var(--font-mono); font-size: 11px;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--app-fg-3);
  display: flex; align-items: center; gap: 8px;
}
.spec-key .req { color: var(--accent); }
.spec-val { font-size: 14px; color: var(--app-fg-1); min-width: 0; }
.spec-val .empty { color: var(--app-fg-4); font-style: italic; font-family: var(--font-mono); font-size: 12px; }

/* ---- Form grid ---- */
.form-grid {
  display: grid; grid-template-columns: 220px 1fr; gap: 16px 24px;
  align-items: start;
}
.form-row { display: contents; }
.form-row > .form-label {
  padding-top: 8px;
  font-family: var(--font-mono); font-size: 11px;
  letter-spacing: 0.06em; text-transform: uppercase;
  color: var(--app-fg-3);
}
.form-row > .form-label .req { color: var(--accent); margin-left: 2px; }
.form-row > .form-label .opt {
  color: var(--app-fg-4); margin-left: 6px;
  font-size: 10.5px; letter-spacing: 0.1em;
}
.form-row > .form-field { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
.form-row > .form-field .help { font-size: 12px; color: var(--app-fg-3); }
.form-row > .form-field .err {
  font-size: 12px; color: var(--gc-err-400);
  display: flex; align-items: center; gap: 4px;
}

/* ---- Stat tiles ---- */
.stat-grid { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; }
.stat {
  border: 1px solid var(--app-border);
  background: var(--app-bg-raised);
  padding: 14px 16px;
  border-radius: 2px;
}
.stat .k {
  font-family: var(--font-mono); font-size: 10.5px;
  letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--app-fg-3);
}
.stat .v {
  font-size: 26px; font-weight: 600; line-height: 1.1;
  letter-spacing: -0.02em; margin-top: 6px;
  font-variant-numeric: tabular-nums;
}
.stat .d { font-size: 12px; color: var(--app-fg-3); margin-top: 4px; font-family: var(--font-mono); }

/* ---- Schema cards (dashboard) ---- */
.schema-card {
  display: block;
  background: var(--app-bg-raised);
  border: 1px solid var(--app-border);
  border-radius: 2px;
  padding: 14px 16px;
  text-decoration: none; color: inherit;
  transition: border-color var(--dur-fast) var(--ease-out), box-shadow var(--dur-fast) var(--ease-out);
}
.schema-card:hover {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent), var(--shadow-2);
}
.schema-card-head {
  display: flex; align-items: center; justify-content: space-between;
  gap: 8px; margin-bottom: 10px;
}
.schema-card-head h3 { margin: 0; font-size: 14px; font-weight: 600; }
.schema-card .meta {
  font-family: var(--font-mono); font-size: 10.5px;
  letter-spacing: 0.08em; color: var(--app-fg-3);
}
.schema-card .field-list {
  list-style: none; margin: 0; padding: 0;
  font-family: var(--font-mono); font-size: 11px;
  color: var(--app-fg-2);
}
.schema-card .field-list li {
  display: flex; justify-content: space-between; gap: 8px;
  padding: 2px 0;
}
.schema-card .field-list li .t { color: var(--app-fg-4); }

/* ---- Empty / loading ---- */
.empty {
  padding: 48px 24px;
  text-align: center;
  color: var(--app-fg-3);
  font-size: 13px;
}
.empty h4 { font-size: 14px; margin: 0 0 4px; color: var(--app-fg-1); }

@keyframes shimmer {
  0%, 100% { opacity: 0.25; }
  50% { opacity: 0.55; }
}
.skel {
  height: 12px; border-radius: 2px;
  background: var(--app-fg-4); opacity: 0.25;
  animation: shimmer 1.6s var(--ease-in-out) infinite;
}

/* ---- Hint bar ---- */
.hintbar {
  display: flex; gap: 16px; align-items: center;
  padding: 8px 16px;
  border-top: 1px solid var(--app-border);
  background: var(--app-bg-2);
  font-size: 11.5px; color: var(--app-fg-3);
  font-family: var(--font-mono);
}
.hintbar .k { display: inline-flex; align-items: center; gap: 6px; }

/* ---- Login split panel ---- */
.login-stage {
  min-height: 100vh;
  display: grid;
  grid-template-columns: 1fr minmax(380px, 460px);
  background: var(--app-bg);
}
.login-left {
  background: var(--gc-ink); color: var(--gc-paper);
  padding: 48px;
  display: flex; flex-direction: column; justify-content: space-between;
  position: relative; overflow: hidden;
}
.login-left h1, .login-left h2, .login-left h3 { color: var(--gc-paper); }
.login-bg {
  position: absolute;
  left: -8%; right: -8%; top: 50%;
  transform: translateY(-50%);
  background-image: url("/logo-mark-white.svg");
  background-repeat: no-repeat;
  background-position: center;
  background-size: contain;
  height: 80%;
  pointer-events: none;
  opacity: 0.05;
}
.login-mark { width: 48px; height: 48px; }
.login-mark img, .login-mark svg { width: 100%; height: 100%; display: block; }
.login-eyebrow {
  font-family: var(--font-mono); font-size: 10.5px;
  letter-spacing: 0.18em; text-transform: uppercase; color: #9A9A9A;
}
.login-hero {
  font-size: 56px; font-weight: 600;
  line-height: 1.0; letter-spacing: -0.02em;
  margin: 16px 0 8px;
  color: var(--gc-paper);
}
.login-tagline {
  font-size: 18px; font-weight: 400;
  line-height: 1.4; max-width: 460px;
  color: #C9C7C1;
}
.login-tagline em { color: var(--accent); font-style: normal; }
.login-foot {
  display: grid; grid-template-columns: 1fr 1fr; gap: 24px; max-width: 520px;
}
.login-foot dt {
  font-family: var(--font-mono); font-size: 10px;
  letter-spacing: 0.14em; text-transform: uppercase;
  color: #6B6B6B; margin-bottom: 4px;
}
.login-foot dd { margin: 0; font-size: 13px; color: #C9C7C1; font-family: var(--font-mono); }
.login-right {
  display: flex; flex-direction: column; justify-content: center;
  padding: 48px;
  background: var(--app-bg);
  position: relative;
}
.login-form-wrap { width: 100%; max-width: 360px; margin: 0 auto; }
@media (max-width: 768px) {
  .login-stage { grid-template-columns: 1fr; }
  .login-left { padding: 32px; }
  .login-hero { font-size: 40px; }
}

/* Tighten heading defaults on the form side so labels/headings flip in dark */
.login-right h1, .login-right h2 { color: var(--app-fg-1); }
"#;

/// Shared in-page error block: destructive-styled region with the message
/// and an optional "Retry" button. Pages wire the button to `query.refetch()`
/// so failed loads don't require a full-page reload.
pub const ERROR_BLOCK: &str = r#"// Generated by schema-forge — edit freely.
import type { ReactNode } from "react"
import { Button } from "@/components/ui/button"

type ErrorBlockProps = {
  /** Short title for the error region (e.g. "Failed to load"). */
  title?: string
  /** The thrown error, or a prebuilt message string. */
  error: unknown
  /** Optional retry callback; renders a button when provided. */
  onRetry?: () => void
  /** Extra context rendered under the primary message. */
  children?: ReactNode
}

export function ErrorBlock({
  title = "Something went wrong",
  error,
  onRetry,
  children,
}: ErrorBlockProps) {
  const message = error instanceof Error ? error.message : String(error)
  return (
    <div className="rounded-md border border-destructive/40 bg-destructive/5 p-4 text-sm">
      <p className="font-medium text-destructive">{title}</p>
      <p className="mt-1 text-destructive/80">{message}</p>
      {children ? <div className="mt-2 text-muted-foreground">{children}</div> : null}
      {onRetry ? (
        <div className="mt-3">
          <Button size="sm" variant="outline" onClick={onRetry}>
            Retry
          </Button>
        </div>
      ) : null}
    </div>
  )
}
"#;
