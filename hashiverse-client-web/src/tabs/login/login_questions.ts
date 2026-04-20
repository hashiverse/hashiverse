// IMPORTANT: These string values are identity-critical — they are hashed to
// derive the user's passphrase. Never change them; only add translations via
// the i18n system using question_to_i18n_key() as the lookup key.

export const LOGIN_QUESTIONS_FIXED = [
	"Your first name?",
	"Your surname?",
	"Your date of birth (yyyy-mm-dd)?",
	"Your favourite number?",
	"Your favourite color?",
	"Your favourite animal?",
	"Profile name (optional)?",
	"A secret phrase (optional)?",
];

export const LOGIN_QUESTIONS_VARIABLE = [
	"Your first language?",
	"Your middle name?",
	"Your last year of high school?",
	"How many siblings do you have?",
	"Name of the street you first lived on?",
	"Name of your first pet?",
	"Name of your first best friend?",
	"City where you were born?",
	"City where your mother was born?",
	"City where your father was born?",
	"Country where you were born?",
	"Your mother's birth year?",
	"Your father's birth year?",
	"Your mother's maiden name?",
	"Your mother's middle name?",
	"Your fathers's middle name?",
];

/** Derives a stable i18n key from a question string, e.g.
 *  "Your first name?" → "your_first_name" */
export function question_to_i18n_key(question: string): string {
	return question
		.toLowerCase()
		.replace(/[^a-z0-9]+/g, "_")
		.replace(/^_+|_+$/g, "");
}
