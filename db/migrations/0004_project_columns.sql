-- `NOT NULL` needs a default to be added to a table that already has rows.
ALTER TABLE `projects` ADD `name` text NOT NULL DEFAULT '';--> statement-breakpoint
ALTER TABLE `projects` ADD `path` text NOT NULL DEFAULT '';--> statement-breakpoint
ALTER TABLE `projects` ADD `created_at` integer NOT NULL DEFAULT 0;--> statement-breakpoint
-- Hand-written: lift the existing fields out of the JSON blob. `created_at`
-- was never recorded, so date the project from its earliest session, which is
-- the closest thing to when it was added.
UPDATE `projects` SET
	`name` = COALESCE(json_extract(`data`, '$.name'), ''),
	`path` = COALESCE(json_extract(`data`, '$.path'), ''),
	`created_at` = COALESCE(
		(SELECT MIN(`created_at`) FROM `sessions` WHERE `sessions`.`project_id` = `projects`.`id`),
		CAST(strftime('%s', 'now') AS INTEGER)
	);
