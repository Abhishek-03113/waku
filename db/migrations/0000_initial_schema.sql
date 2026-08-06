CREATE TABLE `projects` (
	`id` text PRIMARY KEY NOT NULL,
	`position` integer NOT NULL,
	`data` text NOT NULL
);
--> statement-breakpoint
CREATE TABLE `sessions` (
	`id` text PRIMARY KEY NOT NULL,
	`project_id` text NOT NULL,
	`title` text NOT NULL,
	`provider` text NOT NULL,
	`model` text,
	`status` text NOT NULL,
	`created_at` integer NOT NULL,
	`updated_at` integer NOT NULL,
	`last_reply_at` integer,
	`data` text NOT NULL
);
--> statement-breakpoint
CREATE INDEX `sessions_by_project` ON `sessions` (`project_id`,`updated_at`);--> statement-breakpoint
CREATE INDEX `sessions_by_updated_at` ON `sessions` (`updated_at`);--> statement-breakpoint
CREATE INDEX `sessions_by_last_reply_at` ON `sessions` (`last_reply_at`);