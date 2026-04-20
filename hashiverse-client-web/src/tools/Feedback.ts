import {
	IconBulb,
	IconConfetti,
	IconEyeOff,
	IconHeart,
	IconMessage2Exclamation,
	IconMessageCircle,
	IconMessageReport,
	IconRepeat,
	IconShieldExclamation,
	IconShieldOff,
	IconSpeakerphone,
	IconThumbUp,
	IconUserOff,
} from "@tabler/icons-react";
import type React from "react";

export interface FeedbackOption {
	feedback_type: number; // [0,255]
	title: string; // i18n key
	icon: React.FC<{ size?: number }>;
	action: (() => void) | null;
	warning_threshold?: number; // estimated-user count above which a content warning is shown
}

export const FEEDBACK_TYPE_CSAM = 101;
export const FEEDBACK_TYPE_COMMENT = 201;
export const FEEDBACK_TYPE_REPOST = 202;
export const FEEDBACK_TYPE_SEQUEL = 203;

export const FEEDBACKS_WITH_ACTIONS: FeedbackOption[] = [
	{
		feedback_type: FEEDBACK_TYPE_COMMENT,
		title: "feedback.comment",
		icon: IconMessageCircle,
		action: () => {},
	},
	{
		feedback_type: FEEDBACK_TYPE_REPOST,
		title: "feedback.repost",
		icon: IconRepeat,
		action: () => {},
	},
];

export const FEEDBACKS_POSITIVE: FeedbackOption[] = [
	{
		feedback_type: 1,
		title: "feedback.like",
		icon: IconThumbUp,
		action: null,
	},
	{
		feedback_type: 2,
		title: "feedback.love",
		icon: IconHeart,
		action: null,
	},
	{
		feedback_type: 3,
		title: "feedback.confetti",
		icon: IconConfetti,
		action: null,
	},
	{
		feedback_type: 4,
		title: "feedback.insight",
		icon: IconBulb,
		action: null,
	},
];

export const FEEDBACKS_NEGATIVE: FeedbackOption[] = [
	{
		feedback_type: 106,
		title: "feedback.spam",
		icon: IconMessageReport,
		action: null,
		warning_threshold: 10000,
	},
	{
		feedback_type: 102,
		title: "feedback.sensitive",
		icon: IconEyeOff,
		action: null,
		warning_threshold: 200,
	},
	{
		feedback_type: 105,
		title: "feedback.misinformation",
		icon: IconSpeakerphone,
		action: null,
		warning_threshold: 10000,
	},
	{
		feedback_type: 104,
		title: "feedback.harassment",
		icon: IconUserOff,
		action: null,
		warning_threshold: 1000,
	},
	{
		feedback_type: 103,
		title: "feedback.hate_speech",
		icon: IconMessage2Exclamation,
		action: null,
		warning_threshold: 1000,
	},
	{
		feedback_type: 107,
		title: "feedback.scam",
		icon: IconShieldOff,
		action: null,
		warning_threshold: 1000,
	},
	{
		feedback_type: FEEDBACK_TYPE_CSAM,
		title: "feedback.csam",
		icon: IconShieldExclamation,
		action: null,
		warning_threshold: 100,
	},
];
